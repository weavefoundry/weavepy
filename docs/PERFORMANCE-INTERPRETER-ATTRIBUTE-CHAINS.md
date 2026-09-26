# Interpreter attribute chains

The quiet interpreter loop now reads up to eight consecutive cached instance
fields through the root local. It borrows intermediate dictionary or slot
values and retains only the final result. Previously, each intermediate read
created and released an owning reference and returned through dispatch.

Class versions, native-kind exclusions, actual field names, and storage borrows
remain checked. Existing polymorphic dictionary-cache entries can supply an
index, which still requires a matching key. The walk neither fills caches nor
runs callbacks. A miss after two completed reads materializes that prefix;
otherwise ordinary execution handles the chain. Shared storage and observers
retain their existing fallback paths. No native lowering or thresholds change.

## Measurements and tradeoffs

Measurements compare with `a3716e0`, whose production runtime is identical to
`4ae77f8`, on macOS x86-64 with optimized CPython 3.14.5. Process order alternates,
one warmup cycle is discarded, and separate nonempty frozen caches remain
unchanged during timing. Builds, tests, profiles, and other benchmarks finish
before measurement. Ratios are medians of paired cycles; lower is better.

Seven cycles of the existing chain probe, at one million iterations, give:

| Layout and depth | JIT/base work | Interpreter/base work | Interpreter/base CPU |
| --- | ---: | ---: | ---: |
| Dictionary, 2 | 0.945 | 0.787 | 0.859 |
| Dictionary, 4 | 0.898 | 0.598 | 0.674 |
| Dictionary, 8 | 0.963 | 0.465 | 0.532 |
| Dictionary, 9 | 1.019 | 0.513 | 0.567 |
| Slots, 4 | 1.076 | 0.317 | 0.389 |
| Mixed, 4 | 1.046 | 0.409 | 0.493 |

The JIT costs persist. Seven-cycle warmed ratios are 1.025 for dictionary depth
9, 1.098 for slots, and 1.038 for mixed storage. At ten million iterations,
JIT ratios are 1.057, 1.080, and 1.057; interpreter ratios are 0.510, 0.309,
and 0.387. The longer slot case takes 1.020 times CPython's workload time.
These costs are retained, not dismissed as compilation overhead. Diagnostic
traces compile the same operation counts and entry PCs for these cases, with
no frame-reconstruction events. They don't isolate the execution-cost cause.

A two-field Python-created module probe gives JIT/interpreter ratios of
1.034/0.458. These modules use instance storage internally. A separate probe
rooted in the imported `sys` module exercises the native-module fallback and
costs 1.026/1.059. Both include setup, result checks, and cleanup. Native-module
chains remain an optimization opportunity.

The first dictionary-only candidate improved dictionary chains but cost
1.050 for slot-only interpreter execution and 1.081 for mixed storage. It was
superseded before adoption. Its binary, source, and measurements are retained.

The unchanged full suite uses three cycles. Workload geometric means cover
23 fixtures; process metrics include all 24, including startup.

| Metric | JIT/base | Interpreter/base | JIT/CPython |
| --- | ---: | ---: | ---: |
| Workload | 0.982 | 0.985 | 1.077 |
| Process elapsed | 0.980 | 0.982 | 1.350 |
| Process CPU | 0.987 | 0.979 | 1.288 |
| Peak RSS | 1.000 | 0.995 | 1.575 |

Seven-cycle focused application ratios include DeltaBlue 0.952 JIT/0.993
interpreter, Richards 0.992/0.969, calls 0.938/0.945, and float math 1.000/0.988.
The initial full-suite Fib workload cost of 1.036 becomes 0.993 on recheck;
JSON's JIT RSS ratio moves from 1.052 to 0.972. List operations' apparent
RSS improvement shrinks from 0.886 to 0.961, and their workload ratio moves
from 0.935 to 1.015. N-body's repeated JIT workload/RSS ratios are 0.969/0.993.
JSON's interpreter workload recheck costs 1.023. Rechecks don't replace the
full-suite means, and unrelated gains aren't attributed to chain fusion alone.

Thirty-one startup cycles give:

| Launch | JIT elapsed/base | JIT CPU/base | JIT RSS/base |
| --- | ---: | ---: | ---: |
| Normal | 0.993 | 0.987 | 1.010 |
| No site | 0.981 | 1.000 | 1.005 |
| Isolated | 0.992 | 1.005 | 1.005 |
| Imports | 0.994 | 1.003 | 1.010 |

Normal startup still takes 1.505 times CPython's elapsed time, 1.317 times its
CPU, and 1.237 times its RSS. Imports remain at 2.419 elapsed and 2.104 RSS.
The executable is 51,863,672 bytes, 136 bytes larger than the baseline.
These results don't establish overall CPython parity or a general memory gain.

## Validation and records

All 381 VM tests, the full JIT suite, and the small-stack embedding test pass
locally. Clippy passes with warnings denied and the VM's two existing local
exclusions; formatting and the no-default-features check pass. Release
validation passes 85 scripts in JIT, interpreter-only, and GIL-disabled modes
(255 runs), plus 45 probe checks. All 249 AST corpus outcomes match the
accepted binary byte for byte, and 87 selected upstream AST tests pass in each
mode with the existing skips. This isn't the entire upstream AST suite.

The new regression checks field deletion and reordering, dictionary replacement,
class and descriptor mutation, missing values, nullable results, callback counts
and frames, tracing, and ownership. An isolated Rust test proves dictionary,
slot, and mixed paths execute; checks the eight-read bound and completed
prefixes; and verifies mutable-borrow and shared-cell fallback. The Python
fixture also passes CPython.

An exotic string-key callback mismatch found during development already occurs
on the accepted binary; it isn't fixed here. The focused records preserve the
probe and both runtimes' outputs. Windows CI at the preceding `a3716e0` also
has unresolved benchmark and small-stack embedding failures; a local pass
doesn't establish a Windows fix.

Raw timings, immutable binaries, source snapshots, hashes, traces, and logs are
under `target/performance/interpreter-attribute-chain-investigation/`,
`target/performance/interpreter-attribute-chain-slots-investigation/`, and
`target/performance/interpreter-chain-slots-*.json`. The retained executable's
SHA-256 is `861066cb34a932d149b3a0f60be7849f5998dd5e79a488674f76e2bd09a7b989`.
The CLI was built with `cargo build --release -p weavepy-cli --bin weavepy` and
copied only after successful build completion. Benchmark fixtures, baselines,
work sizes, and gates are unchanged.
