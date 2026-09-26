# Constant-time scalar LRU recency

Bounded `functools.lru_cache` wrappers now maintain dense recency links for exact
integer and string keys. Hits update indexes instead of removing an entry and
shifting the rest of the ordered map. Eviction swaps the last entry into the vacant
slot and repairs its links. The dictionary remains the sole owner of Python keys
and results. A bytearray stores indexes, with checked access and no pointer casts.

The first unsupported key restores logical recency order and permanently selects
the existing callback-capable path. Tuple, custom, typed, and keyword keys retain
that path. Recursive insertion preserves the existing entry, and scalar eviction
and clearing release results after publishing a consistent cache. Regressions check
prompt weak-reference clearing, finalizer calling frames, recursive calls, and
exported private buffers. Counter increments now use one dictionary lock, fixing
lost updates when the GIL is disabled. The existing tuple-key cache concurrency
failure remains outside this change.

The compact representation reuses the wrapper's existing typed-state field.
A rejected first representation added a field, crossing the usual dictionary's
capacity boundary from fourteen entries to fifteen. At 10,000 wrappers, that
candidate added roughly 30% peak RSS for scalar caches and 27% for fallback caches.
It was not committed. The accepted representation preserves the field count and
does not change `Object`, `PyInstance`, or native slot layouts.

A second held version still checked recency state twice for fallback calls.
Independent repeats retained 10.1% more JIT work for small custom-key caches and
12.6% for small tuple-key caches. The revised dispatch uses the existing typed-state
read to skip recency handling for typed and permanently disabled caches. Demotion
empties the old buffer for in-flight calls and publishes the disabled state for
future calls, including after clearing.

## Measurement method

Comparisons use accepted `3684cbe`, Intel macOS, and optimized CPython 3.14.5 with
PGO, LTO, and the tail-call interpreter. Process order alternates, warmup is discarded,
and setup, public result checks, clearing, and normal collection remain timed.
Ratios are paired candidate/baseline medians; lower is better. All owned builds,
tests, and profiles finish before timing. Timing controllers run sequentially.
The shared desktop is not a dedicated idle host: the pre-population snapshot
records a Chrome renderer at 48%, Storage at 22%, the Codex renderer at 32%,
and the Codex process at 17% CPU; mediaanalysisd is idle.

The focused matrix covers five cache workloads at capacities 1, 128, and 4,096,
plus eight key paths at capacities 128 and 4,096. All use 10,000 operations and seven
cycles. A separate population probe constructs, exercises, and collects 1,000 or
10,000 empty, scalar, or fallback wrappers. The unchanged application suite and
startup checks provide wider regression coverage. Initial samples are retained
alongside independent repeats.

## Measurements

The seven-cycle focused batch gives these workload ratios against `3684cbe`:

| Workload | Capacity | JIT | Interpreter |
| --- | ---: | ---: | ---: |
| Cycle through cached keys | 128 | 0.692 | 0.693 |
| Cycle through cached keys | 4,096 | 0.121 | 0.115 |
| Eviction on each call | 128 | 0.814 | 0.821 |
| Eviction on each call | 4,096 | 0.160 | 0.149 |
| Unbounded cache | 128 | 0.902 | 0.905 |
| Caching disabled | 128 | 0.897 | 0.907 |

At capacity 4,096, JIT process elapsed/CPU ratios are 0.391/0.350 for cycling
and 0.413/0.373 for eviction. Peak RSS is within 0.3% of baseline. Separate
integer, string, and large-integer key probes give JIT work ratios of 0.161,
0.164, and 0.192, with RSS ratios of 1.017, 1.026, and 1.025.

A separate eleven-cycle check compares the revised runtime with both `3684cbe`
and the held compact-state candidate. Small custom and tuple keys give JIT work
ratios of 0.993 and 0.957 against the accepted baseline, and 0.967 and 0.946
against the held candidate. The held version's earlier 10-13% costs are not
reproduced at that magnitude in this batch, so the full difference should not be
attributed to the revised dispatch. Large transition keys retain 1.043/1.039
JIT/interpreter work in this check, then 1.026/1.007 in the full focused batch.
Size-one eviction has a 1.040 JIT work ratio in the full batch.

Twelve focused cases are repeated for eleven cycles, including every initial
cost above 3%, the earlier fallback concerns, and representative scalar, unbounded,
and uncached gains. Large cycling retains 0.117/0.112 JIT/interpreter work and
large eviction retains 0.160/0.152. Their JIT process elapsed/CPU ratios are
0.386/0.350 and 0.418/0.380. Small custom keys retain 0.979/0.970 and small tuple
keys 0.962/0.947. Large tuple keys are 1.010/1.020. The transition workload
retains a cost: 1.037/1.047 work and 1.024/1.026 process elapsed. Size-one eviction
changes to 1.003/1.034 work. These tradeoffs remain in the report; the change
doesn't improve every cache workload.

The population batch uses seven cycles at each size. At 10,000 wrappers:

| Population | JIT work | Interpreter work | JIT RSS | Interpreter RSS |
| --- | ---: | ---: | ---: | ---: |
| Empty | 0.992 | 1.011 | 0.997 | 1.006 |
| Scalar | 0.983 | 0.978 | 0.960 | 0.976 |
| Fallback | 0.971 | 0.979 | 0.947 | 0.966 |

Eleven-cycle repeats cover all three 10,000-wrapper populations and the
1,000-wrapper scalar case. At 10,000, scalar JIT/interpreter work remains
0.982/0.985 and RSS 0.979/0.960; fallback work remains 0.979/0.970 and RSS
0.947/0.948. Empty wrappers remain near baseline. The 1,000-wrapper scalar case
has work ratios of 1.012/1.015 and RSS near baseline. These gains don't eliminate
the broader construction gap: populated wrappers still take roughly twelve
times CPython work in this probe.

The unchanged 24-fixture suite uses three cycles. JIT geometric means against
baseline are 1.0080 work, 1.0105 elapsed, 1.0092 CPU, and 0.9930 peak RSS;
interpreter means are 0.9967, 0.9865, 0.9881, and 0.9897. Work excludes the
startup-only fixture. These results do not establish a general application
speedup. Seventeen applications are selected for eleven-cycle repeats: every
movement beyond 3% in either direction on any metric, plus call, loop, and float
controls. Their initial Fibonacci, n-body, Richards, and JIT-kernel work costs do
not repeat. Rechecked JIT work ranges from 0.971 to 1.014 and interpreter work
from 0.987 to 1.008. JIT-kernel process elapsed is 1.031 despite 0.991 work and
1.001 CPU. Initial large RSS reductions mostly do not repeat.

Two 31-cycle batches cover all four startup cases. Rechecked JIT elapsed ratios
are 1.001 ordinary, 1.022 without site, 1.009 isolated, and 1.011 imports. The
no-site CPU cost persists: 1.054 initially and 1.088 on recheck. In the second
batch, absolute median CPU changes from 11.592 to 12.318 ms, and elapsed from
24.595 to 24.997 ms; these ratios of separate medians differ from the paired
ratios above. Interpreter no-site elapsed/CPU are 0.970/0.961 on recheck. This
increment does not improve every startup metric.

CPython parity is not achieved. The full-suite JIT geometric means against CPython
are 1.0105 work, 1.3722 elapsed, 1.2310 CPU, and 1.5580 RSS. Large scalar cycling
and eviction still take 7.01 and 6.82 times CPython work. Large unsupported key
paths remain much further behind, and ordinary JIT startup takes 1.687 times
CPython elapsed in the first startup batch and 1.683 on recheck.

## Validation and provenance

The release passes 407 VM tests, embedding's explicit 1 MiB stack case, VM Clippy
with the two established exclusions, no-default compilation, and fourteen
benchmark-tool tests. The frozen CLI passes 345 runs over 115 fixtures in three
modes, 1,048 semantic checks, twenty-eight CPython fixtures, 180 ordinary-population
and 100 GC-population checks, 120 checks each for weak-reference access, saved
calls, and set mutations, 468 LRU probe checks, and thirty-six additional cache
population checks. Five extra GIL-disabled runs pass exact concurrent hit/miss
counts and repeated concurrent clearing of fresh wrappers.

Twenty-three of twenty-four upstream call/descriptor/GC/LRU selections pass.
The GIL-disabled `TestLRUC.test_lru_cache_threaded` tuple-key case reports eight
misses instead of five, reproducing the known baseline failure. Other selected
upstream cases have no unexpected exception output. Full set, weak-set, and
weak-reference suites passed the first scalar implementation; they were not
repeated after compacting its state. Compilation decisions match in fifty-three
probes, three applications, and nine population modes.

The frozen CLI is 51,894,008 bytes, 17,512 more than baseline. Its SHA-256 is
`68b2cc34e1232782d6483818ce34185a7934df37fc9f67d348f78b09409352e2`;
the runtime patch against `3684cbe` is
`7a93e29a383f63da99f985a846f40afcd31e5d1207acbb9356455fb1be1d6d81`.
Sources, binaries, controllers, logs, and raw samples remain under
`target/performance/lru-fallback-state-investigation/` and
`target/performance/scalar-lru-fallback-r3684cbe-*`. Rejected intermediate versions
remain in the separate `lru-scaling-current-investigation`,
`lru-scalar-counters-investigation`, and `lru-compact-state-investigation` directories.
