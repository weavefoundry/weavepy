# Borrow cached module attribute chains

The JIT's borrowed chain helper now accepts existing polymorphic instance
entries and owner-checked native-module entries. Previously, Python-created
modules could have valid polymorphic caches that this helper rejected.
Imported modules also failed its root restriction. Ordinary dynamic reads
then retained intermediate values and repeatedly exhausted the soft pin limit.

The walk checks class versions, ordinary instance kind, module ownership,
actual dictionary keys, and the native module's assigned class. Shared cells
and conflicting mutable borrows reject the borrowed path. It retains only
the final result, or feeds an adjacent exact-integer guard without a pin.
The original root owns every intermediate during this callback-free interval.
Code metadata is borrowed once, without initializing a missing extension.

A diagnostic on 20,000 iterations changes from zero fused hits, seven native
entries, and six pin-pressure exits to 19,975 fused integer reads, one entry,
and zero pin-pressure exits for both module representations. These counts
come from the first candidate, not from timing. The final regression also
proves fusion and absence of pin-pressure exits during its warmed workloads.

## Measurements and tradeoffs

The baseline is runtime `4ea9255`; the implementation was built on `b75c498`,
whose intervening changes add coverage and diagnostics. Measurements run on
macOS x86-64 against optimized CPython 3.14.5 (PGO, LTO, tail-call interpreter,
experimental JIT off). Process order alternates after a discarded warmup.
Each binary has its own nonempty frozen cache, verified unchanged during
measurement. Builds, tests, profiles, and other benchmarks finish first.
Ratios are medians of paired cycles, and lower is better.

Seven cycles at one million iterations include setup, result checks, and
cleanup. Warm cases run the workload once before timing another invocation.

| Workload | JIT/base work | Interp/base work | JIT/base RSS | JIT/CPython work |
| --- | ---: | ---: | ---: | ---: |
| Python module, cold | 0.385 | 1.030 | 0.918 | 1.159 |
| Python module, warm | 0.374 | 0.994 | 0.893 | 1.112 |
| Native module, cold | 0.517 | 0.989 | 0.963 | 1.574 |
| Native module, warm | 0.480 | 1.006 | 0.881 | 1.540 |
| Module fallback probe | 0.300 | 1.023 | 0.820 | 1.844 |
| Alternating dictionary layouts | 1.029 | 1.000 | 1.002 | 4.984 |

The first candidate improved module reads but made ordinary dictionary chains
6.8% slower at depth eight and 12.9% slower at depth sixteen on ten-million-
iteration rechecks. It was held. The revised implementation borrows metadata
once and keeps the primary instance path separate from the polymorphic path.
The final seven-cycle long-chain JIT ratios are 1.004 and 0.990. Warmed ratios
are 0.996 and 0.997. The short Python-module cases cost about 9% relative to
the held candidate; that tradeoff is retained. These measurements do not
isolate the contribution of metadata access from code layout.

Other focused JIT ratios include dictionary depth nine 0.922, slots depth
sixteen 0.999, mixed storage depth sixteen 1.025, class fallback 0.957, and
property fallback 0.962. Application ratios include DeltaBlue 0.983, Richards
0.998, attribute access 1.006, calls 0.991, and float math 1.020.

The unchanged full suite uses three cycles. Workload geometric means cover
23 fixtures; process metrics cover all 24, including startup.

| Metric | JIT/base | Interp/base | JIT/CPython | Interp/CPython |
| --- | ---: | ---: | ---: | ---: |
| Workload | 0.997 | 1.003 | 1.054 | 2.133 |
| Process elapsed | 0.997 | 0.996 | 1.344 | 1.907 |
| Process CPU | 0.997 | 0.997 | 1.273 | 1.868 |
| Peak RSS | 1.004 | 0.994 | 1.576 | 1.342 |

Initial costs include interpreter spectral norm at 1.059 times baseline,
deque workload at 1.038 JIT/1.046 interpreter, pickle JIT workload at 1.031,
and list JIT RSS at 1.111. Seven-cycle rechecks give spectral norm 1.002,
deque 1.011/1.018, and pickle 1.009. List JIT workload is 0.995, but its RSS
still costs 1.023. At 50,000 iterations, list workload is 0.987 and RSS is
1.047; the memory cost is retained. Interpreter RSS rechecks for
pidigits and DeltaBlue are 1.001 and 1.016; JSON JIT RSS is 0.994.
Rechecks do not replace full-suite means or prove unrelated gains come from
chain fusion.

Thirty-one startup cycles give:

| Launch | JIT elapsed/base | JIT CPU/base | JIT RSS/base |
| --- | ---: | ---: | ---: |
| Normal | 1.007 | 1.025 | 0.998 |
| No site | 1.023 | 1.048 | 0.997 |
| Isolated | 1.007 | 1.024 | 0.996 |
| Imports | 1.008 | 1.013 | 1.000 |

A second 31-cycle sweep retains startup costs: normal JIT elapsed/CPU
ratios are 1.010/1.020, and no-site ratios are 1.032/1.060. Isolated and
import elapsed ratios are both about 1.01. Normal startup in the first sweep
remains at 1.514 times CPython's
elapsed time, 1.315 CPU, and 1.230 RSS. Imports remain at 2.456 elapsed and
2.090 RSS. No-site startup is ahead, at 0.904 elapsed and 0.759 RSS.
These results do not establish overall CPython parity.

## Validation and records

All 382 VM tests, the full 71-test JIT suite, and the embedding lifecycle test
on its unchanged 1 MiB stack pass locally. Strict JIT Clippy, VM Clippy with
its two existing exemptions, formatting, and no-default-feature compilation
pass. Release validation passes 86 scripts in JIT, interpreter-only, and
GIL-disabled modes (258 runs), plus 57 probe checks. All 249 AST corpus
outcomes match the accepted binary byte for byte. All 87 selected upstream
AST tests pass in each mode with the existing two skips; this is not the
entire upstream AST suite. Fourteen benchmark-tool tests also pass.

The isolated Rust regression checks owner mismatch, mutable borrows,
polymorphic entries, module-class invalidation, missing metadata without
initialization, and shared-cell revocation after a proven fast-path hit.
Python coverage includes replacement, reordered dictionaries, scalar and
object results, missing fields, callback counts and frames, descriptors,
native modules inside chains, and weak-reference lifetime. It passes CPython.

A preexisting gap remains: generic imported-module reads ignore an assigned
class's custom `__getattribute__` on present dictionary fields. The new helper
rejects assigned classes, and direct Rust coverage proves that rejection.
The failing generic-path example is retained in research storage; it is not
fixed here. At preceding CI head `12edff8`, Windows unit tests pass, including
the small-stack embedding case, but macOS and Windows benchmark gates still
fail. A local pass does not establish a platform fix.

The executable is 51,867,968 bytes, 4,296 bytes larger than the baseline.
Its SHA-256 is `f960190445380a3562de5b5c0e86630843923fdfa5fda33d62c7019301a15826`.
It was built with `cargo build --release -p weavepy-cli --bin weavepy` and
copied only after the successful build exited. Raw results, immutable binaries,
source snapshots, and logs remain under `target/performance/`, including
`module-chain-jit-investigation/`, `cached-chain-metadata-investigation/`,
`module-chain-cache-*.json`, and `cached-chain-metadata-*.json`.
Existing benchmark workloads, CI baselines, and gates are unchanged.
