# Filter tracked weak-reference targets before snapshotting

The weakref-only sweep previously upgraded a slot and cloned an Object for every watched id, then discarded targets already tracked by the cycle collector. The snapshot now checks the borrowed collector index first. Selected targets retain their order and independent ownership, and the sweep still rechecks tracking before clearing. The collector index is borrowed before the registry, matching the existing lock order; both borrows end before callbacks or clearing. There are no new persistent metadata, collection thresholds, JIT decisions, or heap limits.

Profiles of repeated 100,000-object populations attributed about a quarter of active-thread self samples to the old snapshot routine, plus substantial Object cloning, destruction, and tracking checks. These diagnostic samples aren't estimates of removable wall time.

## Measurements

Paired measurements compare accepted `ff18184`, the candidate, and optimized CPython 3.14.5 on Intel macOS. Builds, tests, and profiles ended before timing. Frozen caches and ordinary release settings are preserved. Seven alternating cycles cover five unchanged GC-population modes at 10,000 and 100,000 objects. Each work timer includes retention, promotion, collection, release, and lifetime assertions. Ratios below are candidate/accepted; lower is better.

| Population, 100,000 objects | JIT work time | Interpreter work time | JIT peak RSS | Interpreter peak RSS |
| --- | ---: | ---: | ---: | ---: |
| Self-cycles with weak references | 0.978 | 0.957 | 1.004 | 0.978 |
| Finalizers with weak references | 0.962 | 0.976 | 0.989 | 0.988 |
| Weak references | 0.496 | 0.512 | 0.931 | 0.921 |
| Weak-reference callbacks | 0.480 | 0.458 | 0.933 | 0.910 |
| Frozen self-cycles | 0.970 | 0.962 | 0.985 | 0.983 |

Process CPU confirms the large weakref and callback gains: JIT ratios are 0.505 and 0.483, with interpreter ratios of 0.520 and 0.462. At 10,000 objects the JIT work ratios range from 0.965 to 0.994 and interpreter ratios from 0.863 to 0.999. Memory isn't uniformly lower: the smaller weakref case records 1.019 JIT RSS and the smaller frozen case 1.014 interpreter RSS.

The unchanged 24-fixture suite, three cycles, gives these geometric means. Work time excludes the startup fixture; process metrics include it.

| Metric | JIT/accepted | Interpreter/accepted | JIT/CPython |
| --- | ---: | ---: | ---: |
| Work time | 0.994 | 0.998 | 1.013 |
| Process elapsed | 0.995 | 0.990 | 1.377 |
| Process CPU | 0.997 | 0.989 | 1.246 |
| Peak RSS | 0.997 | 0.996 | 1.554 |

Independent eleven-cycle repeats don't reproduce the initial float-math JIT time, dictionary interpreter time, DeltaBlue JIT RSS, or pi-digits interpreter RSS increases: their ratios are 1.003, 0.971, 1.009, and 0.979. The originals, 1.044, 1.063, 1.058, and 1.059, remain recorded. N-body and spectral norm retain JIT work ratios of 1.034 and 1.032 in the repeats.

Seven-cycle longer runs at 200,000 n-body and 300 spectral-norm work retain JIT/interpreter work ratios of 1.027/1.028 and 1.026/1.009. Process CPU ratios are 1.030/1.030 and 1.033/1.007. These sustained costs remain optimization targets. This revision is retained for the roughly 50% weakref-population work reduction, 7-9% lower memory in those large populations, and approximately unchanged broad-suite metrics; it isn't an improvement in every workload.

Thirty-one-cycle startup measurements give normal/no-site/isolated/import JIT elapsed ratios of 1.005/1.006/1.004/1.008 and CPU ratios of 0.985/0.997/1.013/0.998. Interpreter elapsed ratios are 0.992/0.979/0.981/0.985. Normal JIT startup still takes 1.699 times CPython's elapsed time. The large weakref and callback workloads remain 25.9 and 41.3 times CPython's work time, with 6.8 and 7.7 times its peak RSS. This change doesn't establish CPython parity.

## Validation and reproduction

The new unit test verifies filtering before borrowing excluded slot payloads, no extra target reference, track/untrack transitions, clearing, dead slots, and independent snapshot ownership. All 399 VM tests pass, along with embedding's explicit 1 MiB stack case, VM Clippy with existing exclusions, no-default compilation, and formatting. The frozen CLI passes 315 regression runs across 105 fixtures, 1,048 semantic probes, eighteen CPython fixtures, 180 ordinary-population checks, and 100 GC-population checks. All 53 probe, three application, and nine population compilation decisions are unchanged.

Unmodified upstream CPython 3.14.5 `test_weakref` (137 tests, seven skips), `test_gc` (57 tests, twelve skips), and `test_weakset` (46 tests) pass with the GIL in both JIT and interpreter modes. Weakref and weak-set suites also pass with the GIL disabled. The GIL-disabled GC suite fails `test_trashcan_threads`; three alternating complete GC runs on accepted and candidate binaries reproduce the same failure in all six runs. This existing threaded-finalization gap is retained in the validation record, not counted as passing. Dedicated virtual environments preserve isolated subprocess mode while making the upstream test package importable. Validation wall times are excluded from performance comparisons.

The previously identified mixed bound-method/class lifetime gap remains open. This change doesn't alter callback or finalizer ordering to mask it. The preceding commit's Windows benchmark gate also retains cold sumvm and nested-loop regressions against the merge base; its warmed diagnostic ratios are 0.992 and 1.000. This change doesn't target that compiler cost or alter the gate.

```sh
cargo build --release -p weavepy-cli --bin weavepy
WEAVEPY_INSTANCE_KIND=gc-callback \
WEAVEPY_STDLIB_CACHE="$PWD/target/performance/stdlib-cache" \
python3.14 -B tools/bench_compare.py \
  --base /path/to/ff18184/weavepy --new target/release/weavepy \
  --probe tools/bench_gc_populations.py --work 100000 --samples 7 \
  --frozen-cache-root target/performance/fresh-weakref-caches \
  --out target/performance/weakref-comparison.json
```

The frozen CLI is 51,881,592 bytes, SHA-256
`a5b25184e80136383143f4817821ac290b9ac4da7f92d798ca112a01ba4e7fe6`.
The runtime patch against `ff18184` is
`c09fbd4f43f339c0b2d59c956034bacd3dbfa5113cad8d6f35544bfe9416b864`.
Raw validation, profiles, source snapshots, and population samples remain under
`target/performance/weakref-wrapper-allocation-investigation/`. Broad and
startup measurements use `target/performance/weakref-target-filter-rff18184-*`.
