# Native checks for Python AST fields

Compiling a Python AST previously checked every node's field schema, required
fields, ASDL sum types, and source positions through recursive Python code.
A native walker now accepts ordinary valid trees without those repeated Python
calls. It returns false for invalid or unproved inputs, leaving the original
Python validator responsible for callbacks and error messages. Semantic
validation and conversion to the compiler's internal AST still run.

The walker checks class behavior and metadata, uses callback-free dictionary
probes, and keeps its schema cache within one GIL-held call. Attribute hooks,
descriptors, unusual metadata, cycles, and excessive depth use the Python path.
GIL-disabled execution also uses the Python path. Nothing is cached across
calls, so mutations between compilations remain visible. No unsafe code is added.

## Measurements

Measured against `d25723a` on macOS x86-64 with optimized CPython 3.14.5
(PGO, LTO, tail-call interpreter, experimental JIT off). Seven paired cycles
alternate process order after a discarded warmup. Each binary has its own
nonempty frozen cache, verified unchanged during measurement. No build, test,
profile, or other benchmark overlaps timing. Lower ratios are better; elapsed
time, CPU, and peak RSS cover the complete process.

The compilation probe constructs source, parses it into a Python AST, compiles
at three optimization levels, executes the results, and checks outputs. The
transformation probe constructs functions with loops, comprehensions, and
exception handling; parses them; renames a parameter through NodeTransformer;
then compiles, executes, and checks the transformed program. All this work is
timed, including parsing and validation.

| Probe | Functions | JIT/base workload | Interp/base workload | JIT/base elapsed | JIT/base CPU | JIT/base RSS | JIT/CP workload | JIT/CP RSS |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Compilation | 500 | 0.329 | 0.301 | 0.384 | 0.374 | 0.993 | 8.304 | 2.054 |
| Compilation | 2000 | 0.323 | 0.331 | 0.341 | 0.339 | 0.940 | 7.917 | 2.445 |
| Transformation | 50 | 0.594 | 0.596 | 0.664 | 0.659 | 0.954 | 11.570 | 1.923 |
| Transformation | 200 | 0.594 | 0.584 | 0.619 | 0.615 | 0.975 | 11.818 | 2.337 |

Compilation workloads improve about threefold, and transformation workloads
improve about 41%. These workloads still trail CPython substantially.
The unchanged 20,000-function source-compilation probe gives JIT ratios of
1.009 workload, 1.006 elapsed, 1.006 CPU, and 0.999 RSS. The corresponding
20,000-statement probe gives 0.986, 0.988, 0.986, and 0.944.

The unchanged application suite uses three paired cycles. Workload means cover
23 timed workloads; process means include all 24 fixtures:

| Metric | JIT/base | Interp/base | JIT/CP | Interp/CP |
| --- | ---: | ---: | ---: | ---: |
| Workload time | 1.005 | 1.004 | 1.080 | 2.171 |
| Elapsed | 0.989 | 1.004 | 1.326 | 1.903 |
| CPU | 0.991 | 1.009 | 1.265 | 1.869 |
| Peak RSS | 1.001 | 1.001 | 1.577 | 1.341 |

## Costs and rechecks

Initial costs include JIT nbody workload at 1.054 times baseline, JIT list
workload at 1.030 and RSS at 1.071, and JIT pickle workload at 1.249.
Interpreter sumvm, jitloop, and spectral_norm workloads measure 1.047, 1.091,
and 1.037. The 100-worker probe measures 1.026 JIT workload, 1.019 CPU,
1.022 RSS, and 1.043 interpreter workload. These results remain recorded.

Seven-cycle rechecks retain these workload and JIT RSS ratios:

| Fixture | JIT/base workload | Interp/base workload | JIT/base RSS |
| --- | ---: | ---: | ---: |
| nbody | 0.987 | 1.017 | 1.002 |
| list_ops | 1.008 | 1.010 | 0.975 |
| pickle_bench | 1.002 | 0.995 | 1.002 |
| sumvm | 0.994 | 1.100 | 1.002 |
| jitloop | 1.013 | 1.087 | 1.002 |
| nested_loops | 1.002 | 1.021 | 1.001 |
| spectral_norm | 0.993 | 1.050 | 1.000 |
| float_math | 1.013 | 1.008 | 0.994 |

The initial nbody, pickle, and list-memory costs shrink on recheck. Numeric
interpreter costs persist. Seven-cycle longer runs give sumvm (20 million
iterations) ratios of 0.998 with the JIT and 1.070 in interpreter mode;
jitloop (work 3,000) gives 1.007 and 1.101. The worker recheck gives 1.027
JIT workload and 1.053 interpreter workload. The large AST gains justify this
increment, with these costs retained for further work. The cause of the
unrelated numeric-loop change hasn't been isolated.


A 31-cycle startup sweep gives these candidate/base ratios:

| Mode | JIT elapsed | JIT CPU | Interp elapsed | Interp CPU |
| --- | ---: | ---: | ---: | ---: |
| pass | 0.999 | 1.010 | 1.000 | 1.005 |
| no_site | 0.993 | 1.003 | 0.987 | 0.999 |
| isolated | 0.991 | 0.993 | 0.997 | 1.001 |
| imports | 0.996 | 0.997 | 1.006 | 1.011 |

Startup RSS ratios range from 1.000 to 1.001. No-site startup remains ahead
of CPython in elapsed time, CPU, and RSS. Normal startup and imports remain
behind; normal JIT startup takes 1.447 times CPython's elapsed time and 1.272
times its CPU, with 1.224 times its RSS.

## Validation and reproducibility

All 378 VM tests pass with all features enabled. VM Clippy passes with
warnings denied and the existing `let_and_return` and `cast_ptr_alignment`
exclusions. The VM also checks without default features; formatting passes.
All 79 release regression scripts pass in default JIT, interpreter-only, and
GIL-disabled modes (237 runs). The scalar, source-compilation, short-range,
AST-compilation, transformation, main-module, and REPL probes also pass.

Compiling 249 source files through Python AST objects produces byte-for-byte
identical serialized code, including location and exception metadata. The
native validator accepts 246 of these trees; three use the Python fallback.
A selected set of 87 upstream CPython 3.14.5 AST tests gives the same results
on the baseline and candidate in all three modes and with the native path
forced off. Two tests skip on WeavePy: a resource-dependent standard-library
scan and a CPython-only opcode check. This isn't the entire upstream AST suite.
The new regression fixture checks callbacks, malformed fields and locations,
shared nodes, class and schema mutation, unusual keys, and cycles. A preexisting
AST-subclass lowering limitation remains separate from this change.

Both binaries use `cargo build --release -p weavepy-cli --bin weavepy`.
Candidate SHA-256:
`3f743d0e4e8cca0e926c0bbfd4b4ee7328ef9e44302796c2622c08ba1b126b3a`.
Baseline SHA-256:
`459c208f568ea4a45f0ad1e25b5a20c011220774e10d5fa7bd370ad35954685f`.
The candidate is 51,837,608 bytes, 18,752 bytes larger. It was copied only
after the successful build exited and its source hashes matched. Raw results
are under `target/performance/native-ast-*.json`; source snapshots, manifests,
upstream selection, corpus comparisons, and validation logs are under
`target/performance/ast-phase-investigation/`. The reusable transformation
probe is `tools/bench_ast_transforms.py`.

The preceding commit's macOS ARM CI benchmark gate reports nested_loops at
1.248 times the merge base, with 1.644 on retry and the original retained.
Its suite mean is 0.946 times the merge base. That platform result remains
unresolved and isn't explained or fixed by these local x86-64 AST measurements.
See the [CI job](https://github.com/weavefoundry/weavepy/actions/runs/36022556582/job/107711539267).
Existing benchmark fixtures, CI baselines, and gate thresholds are unchanged.
Universal CPython parity remains unmet.
