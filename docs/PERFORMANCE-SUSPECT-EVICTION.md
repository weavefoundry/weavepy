# Reduce suspect-queue eviction scans

The collector's bounded suspect queue chooses the first entry with the lowest remaining probe budget. An explicit scan now stops at the first zero budget and avoids the aggregate copies emitted for the previous iterator minimum. Eviction subtracts the removed entry's active contribution from the existing exact count instead of recounting the whole map. The queue cap, budgets, dormancy cadence, swap-removal order, handle ownership, and gate publication are unchanged. No metadata or allocation is added.

The accepted release profile attributed 928 of 5,630 active self-cycle samples to `note_suspect`. Dominant instruction offsets map to the minimum-budget loop in its disassembly. This is diagnostic evidence, not a causal estimate of removable runtime.

## Measurements

Seven paired cycles compare accepted 61560f8, the candidate, and optimized CPython 3.14.5 on Intel macOS. Builds, validation, and profiles ended before timing. Unchanged work sizes and frozen caches are used. The work timer includes owner retention, promotion, release, collection, and lifetime assertions. Ratios are candidate/accepted; lower is better.

| Population, 100,000 objects | JIT work | Interpreter work | JIT RSS | Interpreter RSS |
| --- | ---: | ---: | ---: | ---: |
| Self-cycles | 0.848 | 0.855 | 0.988 | 0.998 |
| Finalizers | 0.995 | 0.988 | 0.998 | 0.997 |
| Weak references | 0.986 | 1.014 | 1.012 | 0.991 |
| Weakref callbacks | 0.992 | 1.005 | 0.995 | 0.998 |
| Frozen cycles | 0.876 | 0.862 | 0.997 | 1.001 |

Large self-cycle process CPU ratios are 0.859/0.861 for JIT/interpreter, and frozen-cycle ratios are 0.879/0.871. At 10,000 objects, self-cycle work ratios are 0.866/0.826 and frozen-cycle ratios 0.854/0.841. Both smaller cyclic cases record 1.018 JIT RSS. Smaller weakrefs record 1.014/1.027 work and smaller callbacks 1.029/0.982. The other modes are approximately flat, with these mixed costs retained in the record. Background browser/rendering processes were present and recorded.

The unchanged 24-fixture suite, three cycles, gives these geometric means. Work time excludes the startup fixture; process metrics include it.

| Metric | JIT/accepted | Interpreter/accepted | JIT/CPython |
| --- | ---: | ---: | ---: |
| Work time | 0.977 | 0.996 | 1.012 |
| Process elapsed | 0.980 | 0.989 | 1.365 |
| Process CPU | 0.985 | 0.990 | 1.233 |
| Peak RSS | 1.005 | 0.985 | 1.564 |

Eleven-cycle repeats do not reproduce the initial Fibonacci JIT work, DeltaBlue JIT RSS, or JIT-kernel interpreter elapsed increases: their ratios are 1.005, 0.984, and 0.991, compared with the original 1.053, 1.089, and 1.040. N-body JIT work is 0.996, call-overhead JIT work 0.994, and dictionary interpreter work 1.002, compared with 1.024, 1.027, and 1.028 initially. Repeated n-body JIT RSS remains 1.013. All original samples remain recorded.

Thirty-one-cycle normal/no-site/isolated/import startup gives JIT elapsed ratios of 0.996/1.000/1.001/1.003 and CPU ratios of 1.000/1.018/0.990/1.005. JIT RSS ratios are 1.007/1.008/1.007/1.002. Interpreter elapsed ratios are 0.971/0.949/0.972/0.982. Normal JIT startup still takes 1.672 times CPython's elapsed time.

This revision is retained for the cycle-heavy GC reductions without a repeated material broad-suite slowdown. It doesn't establish CPython parity or improve every metric: large self-cycles still take 17.6 times CPython work and 6.8 times its RSS; callbacks take 39.3 times work and 7.8 times RSS. Small measured memory and startup costs remain optimization targets.

## Validation and provenance

The regression compares the candidate against the prior minimum-and-recount policy over six full-capacity populations and all 1,536 removals. It covers ties, varying active budgets, all dormant, a single zero at the end, u8::MAX, empty state, complete swap order, active counts, and released handles. All 400 VM tests pass, along with embedding's 1 MiB stack case, VM Clippy with established exclusions, no-default compilation, formatting, and fourteen benchmark-tool tests. The frozen release passes 315 runs across 105 regression fixtures, 1,048 semantic probes, eighteen CPython fixtures, 180 ordinary-population checks, and 100 GC-population checks. All 53 probe, three application, and nine population compilation decisions are unchanged.

All nine unmodified upstream CPython 3.14.5 suites completed before broad timing. JIT and interpreter test_weakref (137 tests, seven skips), test_gc (57 tests, twelve skips), and test_weakset (46 tests) pass. GIL-disabled weakref and weak-set suites pass; its GC suite retains only test_trashcan_threads, with 44,042 constructor entries and 44,045 destructor entries. This is the same named failure already reproduced in all six accepted/candidate comparisons for the preceding snapshot-filter change. No new failure or timeout appears. Validation timers are excluded. The previously verified mixed bound-method/class lifetime gap remains open. A separate accepted-binary reflection check also confirms that callback-free weakref wrappers are absent from GC reflection where CPython lists them. No correctness failure is reclassified as a passing test or hidden by changing collection policy.

```sh
cargo build --release -p weavepy-cli --bin weavepy
WEAVEPY_INSTANCE_KIND=gc-cycle \
WEAVEPY_STDLIB_CACHE="$PWD/target/performance/stdlib-cache" \
python3.14 -B tools/bench_compare.py \
  --base /path/to/61560f8/weavepy --new target/release/weavepy \
  --probe tools/bench_gc_populations.py --work 100000 --samples 7 \
  --frozen-cache-root target/performance/fresh-suspect-caches \
  --out target/performance/suspect-comparison.json
```

The frozen CLI is 51,881,456 bytes, SHA-256
`50332b98d423e278759bfe0f9d2f41f5cbe2873410cd97b425ad1caa26be2e15`.
Its runtime patch against `61560f8` is
`5d039ae8b037b41b67a41ef7afe19f8ef1804e63eb1ae3829b257c7433759446`.
Raw samples, diagnostics, source snapshots, and validation stay under
`target/performance/suspect-eviction-investigation/`. Broad, startup, and
repeat samples use `target/performance/suspect-eviction-r61560f8-*`.
