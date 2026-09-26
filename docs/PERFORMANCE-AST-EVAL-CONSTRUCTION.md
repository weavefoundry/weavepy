# Construct eval AST specs once

Eval parsing previously built an unused statement-body spec before constructing
the expression spec it returned. It now constructs only the expression.
Context fix-up, other parse modes, constructors, setters, and callback fallbacks
keep their existing behavior. No unsafe code or dependency change is introduced.

## Measurements

Compared with accepted runtime `9cec550` on macOS x86-64 and CPython 3.14.5
with PGO, LTO, and its tail-call interpreter, with the experimental JIT off.
The parent `2122d13` adds regression coverage and a benchmark probe only.
Seven paired cycles alternate process order after a discarded warmup. Each
binary has separate, nonempty frozen caches, checked for changes. No builds,
tests, profiles, or other benchmarks overlap timing. Lower ratios are better.

The parse probe includes source creation, parsing, result checks, and tree
destruction. Elapsed time, CPU, and peak RSS cover the complete process.

| Eval elements | JIT/base work | Interp/base work | JIT/base elapsed | JIT/base CPU | JIT/base RSS | JIT/CP work |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 500 | 0.828 | 0.836 | 0.902 | 0.892 | 0.950 | 9.718 |
| 2000 | 0.818 | 0.820 | 0.850 | 0.843 | 0.899 | 8.815 |

Eval work improves about 16-18%, but still trails CPython substantially.
Exec and single parsing are roughly unchanged. The initial func_type JIT
ratios are 1.027 at 500 arguments and 1.020 at 2,000; interpreter ratios are
0.972 and 0.967. Compilation, transformation, and ordinary source-compilation
times are also roughly unchanged.

The unchanged application suite uses three paired cycles. Work means cover
23 fixtures; process means cover all 24:

| Metric | JIT/base | Interp/base | JIT/CP | Interp/CP |
| --- | ---: | ---: | ---: | ---: |
| Work | 1.000 | 0.999 | 1.074 | 2.163 |
| Elapsed | 0.999 | 0.995 | 1.375 | 1.941 |
| CPU | 1.002 | 0.995 | 1.296 | 1.889 |
| Peak RSS | 0.995 | 0.993 | 1.569 | 1.346 |

## Costs and controls

All initial results remain recorded. Seven-cycle concern repeats give:

| Case and metric | Initial | Repeat |
| --- | ---: | ---: |
| AST compilation, 500 functions, JIT RSS | 1.071 | 0.935 |
| AST compilation, 2000 functions, JIT RSS | 1.030 | 1.032 |
| Single parsing, 2000 statements, JIT RSS | 1.046 | 1.001 |
| Fannkuch, interpreter work | 1.039 | 1.032 |
| Jitkernels, JIT CPU | 1.046 | 0.997 |
| DeltaBlue, JIT RSS | 1.047 | 1.054 |
| Pickle, JIT work | 1.071 | 1.028 |

A further seven cycles of Fannkuch at 1,000,000 iterations retain JIT work
1.013, interpreter work 1.020, and JIT RSS 1.030. Reversing the binary labels
for DeltaBlue gives accepted/candidate RSS 0.978, equivalent to about 2.3%
more candidate RSS. Its workload times remain near parity.

Identical-executable controls give DeltaBlue JIT RSS 0.986 and large AST
compilation RSS 0.886. These controls show substantial memory-measurement
variation, especially for AST compilation. They don't erase the initial costs
or establish that the candidate's memory differences are caused by noise.

The 31-cycle startup sweep retains small JIT costs: normal elapsed/CPU
1.018/1.011, no-site 1.008/1.020, isolated 1.011/1.008, and imports
1.023/1.023. JIT RSS ratios range from 1.001 to 1.009. Interpreter startup
elapsed ratios range from 0.983 to 0.988 and CPU from 0.974 to 0.991.
Normal JIT startup remains at 1.547 times CPython's elapsed time and 1.231
times its RSS; imports are 2.470 and 2.081. No-site startup remains ahead
at 0.923 elapsed, 0.589 CPU, and 0.762 RSS.

## Validation and reproducibility

All 383 VM tests, the complete 71-test JIT suite, the unchanged 1 MiB embedding
test, strict JIT Clippy, VM Clippy with its two existing exclusions, formatting,
no-default-feature compilation, and fourteen benchmark-tool tests pass.
Release validation passes 87 scripts in three modes (261 runs), plus 81
additional probes. All 249 AST corpus outcomes match the accepted runtime
byte for byte. The selected 87 upstream AST tests pass in each mode with the
existing two skips; this isn't the complete upstream AST suite.

All 30 focused context lists match CPython in each mode, and all complete trees
preserve accepted behavior. Twenty-nine complete trees match CPython. The
existing type-comment end-column difference remains: WeavePy reports 12 and
CPython reports 36. Existing allocation and singleton differences also remain.

The executable is 51,868,080 bytes, 112 more than the baseline. SHA-256:
`440c2baa1440dda31a05bea788e5b1cc766cf825f8e13ff3bc14e3d0f6302e3f`.
Build with `cargo build --release -p weavepy-cli --bin weavepy`. The executable
was copied only after its successful build closed, and source hashes still
match the measured inputs. No CI gate or baseline changed.

Controllers, source snapshots, controls, and logs remain under
`target/performance/ast-eval-once-investigation/`; raw results are
`target/performance/ast-eval-once-*.json`. Reproduce parsing with
`WEAVEPY_AST_PARSE_MODE=eval python3.14 -B tools/bench_compare.py --base BASE
--new NEW --samples 7 --probe tools/bench_ast_parse_contexts.py --work 2000
--frozen-cache-root target/performance/REPRO-CACHE --out target/performance/REPRO.json`.
Preserve writable standard-library staging and separate frozen caches.
