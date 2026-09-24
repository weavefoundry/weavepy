# Native construction of Python AST nodes

Building Python AST objects from the parser's intermediate dictionaries and
lists now uses a native traversal for ordinary specs. Constructors, attribute
setters, and iterators still use VM dispatch. The original Python builder
handles replaced helpers, unusual inputs, observers, and GIL-disabled execution.
Each recursive step checks the current helper and builtin bindings; constructors
and setters can change those bindings for subsequent nodes.

The traversal hashes its fixed lookup names once, compares guard values without
cloning them, and reuses the Python helper's exact `_type` literal. It preserves
recursion accounting, periodically returns to the evaluator, and keeps no
persistent class or value cache. Partial results and discarded call temporaries
use the existing VM cleanup paths, preserving the tested finalizer order and
timing on success and failure. No unsafe code is added.

## Measurements

Compared with the runtime at `d216b4f` on macOS x86-64 and optimized CPython
3.14.5 (PGO, LTO, tail-call interpreter, experimental JIT off). The parent
`94f7b21` changes only a signal-test counter. Seven paired cycles alternate
process order after one discarded warmup. Every binary uses its own nonempty
frozen cache, checked for changes during measurement. No builds, tests, profiles,
or other benchmarks overlap timing. Lower ratios are better. Elapsed time, CPU,
and peak RSS cover the complete process.

Both AST probes time source creation, parsing, compilation, execution, and
result checks. The compilation probe compiles at three optimization levels.
The transformation probe also rewrites functions containing loops,
comprehensions, and exception handling through NodeTransformer.

| Probe | Functions | JIT/base workload | Interp/base workload | JIT/base elapsed | JIT/base CPU | JIT/base RSS | JIT/CP workload |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Compilation | 500 | 0.803 | 0.798 | 0.845 | 0.838 | 0.984 | 5.982 |
| Compilation | 2000 | 0.845 | 0.791 | 0.858 | 0.855 | 1.037 | 6.549 |
| Transformation | 50 | 0.840 | 0.837 | 0.876 | 0.860 | 0.962 | 9.179 |
| Transformation | 200 | 0.814 | 0.815 | 0.829 | 0.829 | 1.000 | 9.782 |

These workloads improve about 16-21%, but still trail CPython substantially.
The 20,000-function source-compilation probe gives JIT/base ratios of 0.993
workload, 0.991 elapsed, 0.999 CPU, and 0.994 RSS. The 20,000-statement probe
measures 0.972, 0.990, 1.005, and 1.000; its interpreter workload is 1.026.
The 100-worker probe measures 0.996 JIT workload and 0.989 interpreter workload.

The unchanged application suite uses three paired cycles. Workload means cover
23 fixtures; process means cover all 24:

| Metric | JIT/base | Interp/base | JIT/CP | Interp/CP |
| --- | ---: | ---: | ---: | ---: |
| Workload | 0.999 | 1.002 | 1.073 | 2.223 |
| Elapsed | 0.994 | 1.006 | 1.351 | 1.963 |
| CPU | 1.004 | 1.008 | 1.283 | 1.926 |
| Peak RSS | 0.995 | 1.005 | 1.564 | 1.339 |

## Costs and rechecks

Initial JIT workload ratios include fib at 1.073 and jitkernels at 1.044.
Interpreter float_math is 1.037. JIT peak RSS is 1.071 for pidigits and
1.050 for deltablue. These results remain recorded. Seven-cycle rechecks give:

| Fixture | JIT/base workload | Interp/base workload | JIT/base RSS |
| --- | ---: | ---: | ---: |
| fib | 1.003 | 1.015 | 0.996 |
| jitkernels | 1.016 | 1.014 | 0.999 |
| float_math | 1.006 | 1.019 | 0.991 |
| pidigits | 1.005 | 1.007 | 1.020 |
| deltablue | 1.015 | 0.998 | 1.023 |

The larger initial costs shrink on recheck. The 2,000-function AST recheck
retains workload ratios of 0.847 with the JIT and 0.800 in interpreter mode;
JIT peak RSS is 0.995, so its initial increase doesn't repeat.

Two 31-cycle startup sweeps give the following candidate/base ratios. Each
cell shows the initial result followed by the repeat:

| Mode | JIT elapsed | JIT CPU | Interp elapsed | Interp CPU |
| --- | ---: | ---: | ---: | ---: |
| pass | 1.009 / 1.005 | 1.005 / 0.989 | 1.006 / 1.001 | 1.004 / 1.006 |
| no_site | 1.007 / 1.018 | 1.003 / 1.044 | 1.001 / 1.005 | 0.978 / 1.005 |
| isolated | 1.019 / 1.008 | 0.998 / 1.014 | 1.005 / 1.009 | 1.030 / 0.990 |
| imports | 1.005 / 1.003 | 1.010 / 1.002 | 0.998 / 1.009 | 0.996 / 1.013 |

Most startup changes are small, but no-site JIT CPU is 4.4% higher in the
repeat, compared with 0.3% initially. This mixed result is retained as a cost;
its cause hasn't been isolated. Startup RSS ratios remain between 0.999 and
1.002. No-site startup is still ahead of CPython in elapsed time, CPU, and RSS.
Normal startup remains slower: the repeat measures 1.452 times CPython's
elapsed time, 1.302 times its CPU, and 1.221 times its RSS with the JIT enabled.

## Validation and reproducibility

All 379 VM unit tests pass. Clippy passes with warnings denied and the two
existing `let_and_return` and `cast_ptr_alignment` exclusions. The VM checks
without default features, with its existing unused-function warning; formatting
passes. All 83 release regression scripts pass with the JIT enabled, disabled,
and with the GIL disabled (249 runs), along with 45 scalar, compilation,
short-range, AST, transformation, main-module, and interactive probe checks.

Compiling 249 source files through Python AST objects gives byte-for-byte
identical serialized code, including location and exception metadata. The
selected 87-test upstream CPython 3.14.5 AST set passes in all three modes,
with the same two existing skips. This isn't the complete upstream AST suite.
A caller-frame diagnostic confirms that the native construction path runs.

The new regression fixtures compare native and forced-Python behavior for
mutable registries, helpers, code, builtins, constructors, setters, descriptors,
custom keys, subclasses, iteration mutation, recursion, and tracing. Cleanup
checks cover partial results, ignored child values, discarded setter results,
constructor callables, and replaced recursive builders, with events checked
both before and after collection in both JIT modes.

The candidate binary is 51,863,536 bytes, 25,928 bytes larger than the baseline.
SHA-256: `1ce2cf59568553ed7cd0b77a80cc57f9d10c18ec17ffd17e978129b7cd4a1c90`.
The baseline SHA-256 is
`3f743d0e4e8cca0e926c0bbfd4b4ee7328ef9e44302796c2622c08ba1b126b3a`.
Source snapshots, build inputs, validation logs, and controllers are under
`target/performance/ast-build-cleanup-investigation/`. Raw timing data is under
`target/performance/cleanup-build-*.json`.

Build with `cargo build --release -p weavepy-cli --bin weavepy`. Reproduce the
paired comparisons with `python3.14 -B tools/bench_compare.py --base BASE
--new NEW --samples 7 --probe tools/bench_ast_compilation.py --work 2000
--frozen-cache-root target/performance/REPRO-CACHE --out target/performance/REPRO.json`.
Use `tools/bench_ast_transforms.py` for the transformation probe and
`tools/bench_startup.py` for startup. Preserve writable standard-library staging
and keep each binary's frozen cache separate.
