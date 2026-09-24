# Compile owned syntax trees

The compiler previously cloned every syntax tree before constant folding.
Source execution, imports, the REPL, and the `compile`, `exec`, and `eval`
builtins already owned newly parsed or converted trees. They now transfer
ownership to the compiler, which folds those trees in place. The existing
borrowed Rust APIs retain their signatures and leave their inputs unchanged.
Validation order, compile options, and bytecode contents are preserved.

The main-module runner also releases its syntax tree before starting the
interpreter. Previously, the original tree remained live during execution.
Python AST inputs still undergo conversion to a separate internal tree;
compiling one doesn't mutate the caller's Python nodes.

## Measurements

Measured against `ec9cdb1` on macOS x86-64 with optimized CPython 3.14.5
(PGO, LTO, tail-call interpreter, experimental JIT off). Seven paired cycles
alternate process order after a discarded warmup. Each WeavePy binary uses
its own nonempty frozen cache, verified unchanged during measurement. No
build, test, profile, or other benchmark overlaps timing. Lower ratios are
better. Elapsed time, CPU, and RSS cover the complete process.

The function probe constructs and compiles distinct wrappers, calls each four
times, checks the result, and retains the functions. The statement probe
constructs, compiles, executes, and checks a sequence of additions. All of
that work is inside the workload timer:

| Probe | Size | JIT/base workload | Interp/base workload | JIT/base elapsed | JIT/base CPU | JIT/base RSS | JIT/CP workload | JIT/CP RSS |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Functions | 2,000 | 0.902 | 0.912 | 0.941 | 0.954 | 0.909 | 1.069 | 1.082 |
| Functions | 10,000 | 0.925 | 0.921 | 0.941 | 0.941 | 0.833 | 1.061 | 1.000 |
| Functions | 20,000 | 0.934 | 0.922 | 0.932 | 0.938 | 0.804 | 1.036 | 0.987 |
| Statements | 2,000 | 0.838 | 0.843 | 0.986 | 0.971 | 0.916 | 0.542 | 0.989 |
| Statements | 10,000 | 0.817 | 0.826 | 0.922 | 0.916 | 0.816 | 0.679 | 0.817 |
| Statements | 20,000 | 0.829 | 0.858 | 0.894 | 0.888 | 0.788 | 0.715 | 0.882 |

Function workloads improve 7-10%, with 9-20% lower peak RSS. Statement
workloads improve 16-18%, with 8-21% lower peak RSS. The larger statement
cases beat CPython in workload time, process elapsed time, CPU, and RSS;
the 2,000-statement case still costs 1.248 times CPython's process elapsed
time and 1.105 times its CPU. Function workloads remain slower than CPython.

Generated main modules define functions and allocate a 16 MiB payload,
touching each page. Their internal execution timer excludes parsing and
compilation, so process metrics are the primary comparison:

| Functions | JIT/base elapsed | JIT/base CPU | JIT/base RSS | Interp/base elapsed | Interp/base RSS | JIT/CP elapsed | JIT/CP CPU | JIT/CP RSS |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1,000 | 0.962 | 0.970 | 0.937 | 0.961 | 0.936 | 1.244 | 1.138 | 1.067 |
| 10,000 | 0.923 | 0.919 | 0.826 | 0.910 | 0.852 | 1.000 | 0.976 | 1.104 |
| 20,000 | 0.949 | 0.949 | 0.679 | 0.932 | 0.665 | 0.964 | 0.939 | 1.009 |

The 20,000-function process uses about 32% less peak RSS. Its execution-only
timer costs 1.078 times baseline with the JIT and 1.103 in interpreter mode;
the end-to-end result still improves. The 10,000-function execution-only
costs are 1.036 and 1.038. These costs are retained, not removed from the
comparison by treating compilation as untimed setup.

The Python-AST probe builds a tree, compiles it at three optimization levels,
executes each result, and checks outputs. At 500 and 2,000 functions, JIT
workload ratios are 1.003 and 0.993; RSS ratios are 0.909 and 0.962. It still
takes about 23.7-23.8 times CPython's workload time and 2.0-2.7 times its RSS.
Removing the internal clone doesn't address most of that path's cost.

The unchanged 100-worker probe improves to 0.937 times baseline workload,
0.945 elapsed, 0.941 CPU, and 0.969 RSS. It remains at 1.479 times CPython's
workload, 1.542 elapsed, 1.532 CPU, and 1.797 RSS.

The unchanged 24-fixture application suite uses three paired cycles. Workload
means cover the 23 timed workloads; process means include startup:

| Metric | JIT/base | Interp/base | JIT/CP | Interp/CP |
| --- | ---: | ---: | ---: | ---: |
| Workload time | 0.993 | 0.987 | 1.095 | 2.168 |
| Elapsed | 1.003 | 0.987 | 1.390 | 1.932 |
| CPU | 1.004 | 0.987 | 1.324 | 1.897 |
| Peak RSS | 0.997 | 0.990 | 1.570 | 1.340 |

## Costs and rechecks

Initial costs include floating math at 1.048 times baseline JIT workload,
interpreter attribute access at 1.055, interpreter JSON at 1.030, pidigits
JIT RSS at 1.041, and interpreter JSON RSS at 1.063. Conversely, interpreter
sumvm and jitloop improve to 0.870 and 0.898. The seven-cycle rechecks give
these workload and JIT RSS ratios:

| Fixture | JIT/base | Interp/base | JIT/base RSS |
| --- | ---: | ---: | ---: |
| float_math | 0.974 | 0.996 | 1.002 |
| attr_access | 1.017 | 1.059 | 1.004 |
| json_bench | 1.011 | 1.002 | 1.015 |
| pidigits | 1.013 | 1.003 | 0.968 |
| pyaes | 1.015 | 1.016 | 0.989 |
| sumvm | 0.997 | 0.887 | 1.001 |
| jitloop | 0.980 | 0.922 | 1.002 |
| jitkernels | 1.024 | 0.990 | 0.994 |

Interpreter attribute access still costs about 6%; its process CPU ratio is
1.047. The original floating-math, interpreter-JSON, and pidigits-memory
costs shrink on recheck. The original measurements remain in the report.
Seven-cycle longer runs give sumvm (20 million iterations) ratios of 0.995
with the JIT and 0.891 in interpreter mode; jitloop (work 3,000) gives 1.014
and 0.918. No causal claim is made for unrelated interpreter-loop gains.

The repeated 20,000-function main-module check gives JIT ratios of 0.927
elapsed, 0.922 CPU, and 0.672 RSS. Execution-only costs persist at 1.058
with the JIT and 1.074 in interpreter mode. End-to-end gains justify keeping
the change, with these execution costs recorded.

Two independent 31-cycle startup sweeps retain these candidate/base ratios:

| Mode | JIT elapsed 1 / 2 | JIT CPU 1 / 2 | Interp elapsed 1 / 2 | Interp CPU 1 / 2 |
| --- | ---: | ---: | ---: | ---: |
| pass | 1.013 / 1.013 | 1.035 / 1.028 | 0.985 / 0.992 | 0.991 / 1.006 |
| no_site | 1.013 / 1.008 | 0.986 / 1.069 | 0.971 / 0.985 | 1.005 / 0.925 |
| isolated | 1.021 / 1.020 | 1.025 / 1.018 | 0.984 / 0.988 | 0.965 / 0.997 |
| imports | 1.012 / 1.008 | 1.021 / 0.999 | 0.994 / 1.003 | 0.994 / 0.993 |

Normal and isolated JIT startup costs persist. No-site CPU varies from a
1.4% gain to a 6.9% cost, so no improvement is claimed there. JIT RSS falls
about 0.6-0.8% for normal/isolated startup, rises 0.7-0.9% without site,
and is approximately unchanged for imports. No-site startup remains ahead
of CPython on elapsed, CPU, and RSS; ordinary startup and imports do not.

## Validation and reproducibility

All 40 compiler tests, 377 VM tests, and three library tests pass. Compiler
Clippy passes with warnings denied; VM Clippy passes with the existing
`let_and_return` and `cast_ptr_alignment` exclusions. The CLI test target
builds but contains no unit tests; subprocess REPL checks cover the changed
interactive path. Formatting passes.

All 78 release regression scripts pass in default JIT, interpreter-only, and
GIL-disabled modes (234 runs). Twelve scalar probes, six compilation probes,
nine short-range probes, six Python-AST probes, three main-module probes, and
three REPL checks also pass. Compiling 248 source files produces byte-for-byte
identical serialized code objects, including location and exception metadata.
The new tests cover all compile modes and optimization levels, future flags,
closures, validation errors, and reuse of mutated Python AST inputs.

Both executables use `cargo build --release -p weavepy-cli --bin weavepy`.
The candidate SHA-256 is
`459c208f568ea4a45f0ad1e25b5a20c011220774e10d5fa7bd370ad35954685f`;
the baseline is
`65fadbedd4e3b5317aaadec46bf1000d8e115748858f1e5a7c31f066e42940d8`.
The candidate is 51,818,856 bytes, 312 bytes smaller. It was copied only
after the successful build exited and its source hashes matched. Raw
measurements are under
`target/performance/owned-ast-*.json`; source hashes, corpus comparisons,
validation logs, generated main modules, and controllers are under
`target/performance/owned-ast-compilation-investigation/`. Reusable probes
are `tools/bench_ast_compilation.py` and `tools/make_compile_main_probe.py`.

This change doesn't establish universal CPython parity. Existing benchmark
fixtures, CI baselines, and gate thresholds are unchanged.
