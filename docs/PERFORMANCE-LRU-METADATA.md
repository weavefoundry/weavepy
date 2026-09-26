# Borrowed LRU metadata lookups

LRU wrappers repeatedly allocated temporary Python strings to read their
private fields. An eight-second profile of the preceding scalar implementation
at capacity 128 attributed about 31% of active worker samples to `lru_get`,
including its callees. The recency-link update itself accounted for about 1%.

This increment uses the existing borrowed `StrKey` probe for those reads and
for occupied counter and recency-state entries. Ordinary occupied entries
no longer allocate a key. Stored dictionary keys, callback-capable equality, lock scope,
mutation stamps, and deferred owner tracking are preserved. Counter updates
remain atomic. No wrapper fields, object layouts, JIT thresholds, benchmark
workloads, or baselines change.

## Measurements

The baseline is `a7a14bf`, with `09a56e9` as an additional focused comparator.
Measurements use Intel macOS and optimized CPython 3.14.5 with PGO, LTO, and
the tail-call interpreter. Process order alternates, warmup is discarded, and
setup, validation, and normal collection stay timed. All owned builds,
correctness checks, and profiles are separate from timing.

This is a shared desktop. The initial snapshot recorded `mediaanalysisd`
at 100.6% CPU and the Codex renderer at 34.4%. Before repeats, `mds_stores`
used 154.2%, media services about 27%, the renderer 31.2%, and Codex 17.3%.
The results aren't idle-host measurements. Original results and repeats are
both retained.

The focused matrix has 31 workload/key/capacity combinations at seven cycles.
Fifteen eleven-cycle repeats include every initial cost above 3% in any
mode or metric, plus representative controls. Lower ratios are better.

| Repeated workload | JIT work / baseline | Interpreter work / baseline | JIT work / CPython |
|---|---:|---:|---:|
| Scalar cycling, capacity 128 | 0.862 | 0.852 | 10.109 |
| Scalar cycling, capacity 4,096 | 0.859 | 0.854 | 6.316 |
| Scalar eviction, capacity 4,096 | 0.892 | 0.919 | 6.086 |
| Tuple keys, capacity 4,096 | 1.025 | 1.006 | 32.942 |
| Typed keys, capacity 4,096 | 0.986 | 1.001 | 31.680 |

Every initial focused cost above 3% disappears on recheck, including the
5.1% typed-key JIT work cost and 3.8% transition-key interpreter cost.
Repeated transition-key work is 0.956/1.016 of baseline. The large scalar
cycling and eviction JIT elapsed ratios are 0.978 and 0.984; startup limits
their complete-process gains. Their JIT RSS ratios are 1.013 and 1.016.
This isn't a general memory-use improvement, and large fallback caches
remain a major performance gap.

Six population cases use seven cycles; four eleven-cycle repeats cover the
initial small-fallback process-time cost and all three 10,000-wrapper controls.
At 10,000 wrappers, repeated empty/scalar/fallback JIT work ratios are
1.003/0.986/0.996 and RSS ratios are 1.006/0.996/0.997. Interpreter work ratios
are 1.001/1.010/0.987 and RSS ratios are 1.007/0.994/1.010. The initial
1,000-wrapper fallback interpreter elapsed/CPU costs, 1.084/1.048, become
1.002/0.986. No population cost above 3% remains on recheck.

The unchanged 24-fixture suite uses three cycles. Work excludes the
startup-only fixture; process metrics include it.

| Initial geometric mean | Work | Process elapsed | Process CPU | Peak RSS |
|---|---:|---:|---:|---:|
| JIT / baseline | 0.9813 | 0.9724 | 0.9771 | 1.0011 |
| Interpreter / baseline | 1.0356 | 1.0229 | 1.0227 | 1.0020 |
| JIT / CPython | 1.0082 | 1.3568 | 1.2220 | 1.5587 |

Twenty-three fixtures are repeated for eleven cycles after initial movements
above 3% in either direction, with loop, call, and float controls. Their JIT
work/elapsed/CPU/RSS means are 0.9973/0.9989/1.0028/1.0003; interpreter means
are 0.9959/0.9957/0.9941/0.9975. These are selected-fixture means, not a new
24-fixture result. No general application speedup is claimed.

Individual repeated costs remain. JIT work ratios are 1.032 for attribute
access, 1.042 for float math, and 1.039 for string methods. Interpreter work
ratios are 1.041 for attribute access, 1.053 for Fibonacci, 1.059 for JIT
kernels, 1.051 for n-body, and 1.040 for pickle. Interpreter JIT-kernel
elapsed/CPU ratios are 1.039/1.041, and n-body ratios are 1.032/1.038.
JIT nested-loop CPU is 1.032; the startup fixture's JIT CPU is 1.051.
Several costs have broad paired ranges, so their cause remains uncertain.
They aren't discarded because the overall means are near baseline.

Two dedicated 31-cycle startup batches are separate from that file-based
startup fixture. The first no-site JIT elapsed ratio, 1.033, becomes 1.004
on recheck. Repeated JIT elapsed ratios for ordinary, isolated, and import
startup are 1.001, 1.000, and 1.004. Their CPU ratios are 0.988, 1.024, and
1.000; no-site CPU is 1.011. Ordinary JIT startup still takes 1.672 times
CPython elapsed. No startup speedup is claimed.

## Validation and retained evidence

The change passes 408 VM tests, embedding's explicit 1 MiB stack case, VM
Clippy with established exclusions, no-default compilation, scoped formatting,
and the repository artifact check. The frozen CLI passes 30 selected
regression runs across three modes, four CPython oracle runs, five additional
GIL-disabled counter/clear runs, 468 cache-matrix checks, and 36 cache-population
checks. The new regression covers equal custom metadata keys, stored-key
identity, resets, missing counters, and missing scalar state.

Twenty-three of twenty-four upstream suite selections pass. The existing
GIL-disabled tuple-key `TestLRUC.test_lru_cache_threaded` failure remains,
with thirteen misses instead of five in this run. There are no ignored
exceptions in those logs. Compilation decisions match the baseline in
53 probes and three applications.

After all timing controllers closed, matching eight-second profiles sampled
5,576/5,674 active JIT/interpreter worker stacks. Inclusive `lru_get` fractions
are now 20.7%/23.7%, compared with 31.0%/31.4% in the `09a56e9` profile; counter
updates are 4.4%/4.0%, compared with 7.9%/7.7%. These overlapping fractions
aren't additive speedups. The CLI's waiting main thread is excluded. Both
owned workload children were terminated and reaped after sampling. Profiles
remain under `target/performance/borrowed-lru-call-profile/`.

The frozen executable is 51,894,168 bytes, 160 more than the baseline. Its
SHA-256 is
`76dd51765dec3bf3d57c24bc89e0d4a1f9b33354c7e019b14de923a02c2ab019`;
the runtime patch SHA-256 is
`3bd07cdc67aaa66b2f4d7791540f2079ba60d34305b8ac3c513ca5ac908dc45a`.
Raw data, snapshots, and logs remain under
`target/performance/lru-borrowed-metadata-investigation/`. The full-suite and
initial startup files are `target/performance/lru-borrowed-ra7a14bf-full.json`
and `target/performance/lru-borrowed-ra7a14bf-startup.json`; the immutable CLI
is `target/performance/weavepy-lru-borrowed-metadata`.
