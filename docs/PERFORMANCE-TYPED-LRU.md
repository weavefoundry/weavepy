# Constant-time recency for bounded typed LRU caches

Bounded `functools.lru_cache(typed=True)` calls can now use dense recency links
when their keys contain exact integers, strings, tuples, and the corresponding
builtin argument classes. Previously, each such hit shifted an ordered map,
so its cost grew with cache capacity. Admission still visits at most 64 objects
and eight tuple levels; unsupported keys retain the callback-capable path.

Typed mode uses an immediate initial tag and a one-element tuple around active
recency storage. Unused wrappers require no additional state allocation. No
wrapper field or global object variant is added. Calls read the mode through a
borrowed dictionary lookup, and rare demotion preserves the typed tag while
holding the cache lock. The fixed-size tuple constructor avoids a temporary
vector when storage is initialized.

Only the exact builtin `int`, `str`, and `tuple` class identities qualify as
native type leaves. Custom metaclass hashing stays outside cache borrows.
Incoming and matched stored keys retain their guards, and demotion checks all
stored owners before rebuilding logical order. Clear preserves mode and
serializes with first initialization and recency updates. Evicted values leave
the paired borrow before finalizer dispatch.

## Validation

The candidate passes all 408 VM tests, embedding's explicit 1 MiB stack test,
VM Clippy with the established exclusions, compilation without default
features, and scoped formatting. The typed fixture uses an independent recency
oracle across seven key forms and seven capacities. It also covers type
separation through demotion and clear, nested equality, stored owners,
recursive misses, hashing errors and callbacks, admission bounds, exported
storage, private stored-key tampering, and finalizer frames.

The frozen CLI passes 42 regression runs, five additional GIL-disabled
counter/clear runs, 468 cache-matrix checks, and 100 population checks. All 24
selected upstream call, descriptor, GC, and LRU groups pass, including the
full 31-test `TestLRUC` group in three modes. Twelve native SQLAlchemy/Alembic
checks pass, including compiled imports, synchronous and asynchronous queries,
and migrations. Native extensions can reenable the GIL in those database
launches; the separate cache fixtures exercise GIL-disabled concurrency.
Compilation decisions match the accepted binary in 53 probes and three
applications. Selected logs contain no ignored exceptions.

## Measurement method

Comparisons use accepted `b102e92`, optimized CPython 3.14.5 with PGO, LTO,
and the tail-call interpreter, and Intel macOS. Process order alternates,
warmup is discarded, and setup, checks, and normal collection stay timed.
All owned build, validation, and diagnostic controllers finish before timing.
Desktop processes remain active, so this is not a dedicated idle machine.
No workload, baseline, gate, or JIT admission setting changes.

The initial matrix includes all 31 existing cache cases, six ordinary wrapper
populations, and eight cases from a separate typed-population probe, each with
seven cycles. The unchanged 24-application suite uses three cycles. Follow-up
comparisons include the held first candidate, retain its measurements, and
repeat its persistent cost cases as well as new costs and relevant controls.

## Results

Sixteen focused cases are repeated for eleven cycles. Ratios below are
candidate work divided by `b102e92` work; lower is better.

| Workload | JIT | Interpreter | JIT / CPython |
| --- | ---: | ---: | ---: |
| Typed, capacity 128 | 0.797 | 0.800 | 8.391 |
| Typed, capacity 4,096 | 0.188 | 0.189 | 6.423 |
| Scalar cycling, capacity 1 | 0.951 | 0.952 | 7.901 |
| Scalar cycling, capacity 4,096 | 0.953 | 0.971 | 5.684 |
| Unbounded, 128-key workload | 0.967 | 0.949 | 8.966 |
| Custom keys, capacity 4,096 | 0.990 | 0.986 | 26.423 |

The large typed case initially measures 0.198/0.193 in JIT/interpreter
work. Repeated process elapsed ratios are 0.421/0.415, CPU ratios are
0.386/0.380, and RSS ratios are 1.015/1.014. Capacity-128 typed work initially
measures 0.815/0.819. These gains remove capacity-dependent shifting for
admitted keys; they do not establish CPython parity.

Costs are retained. Initial focused increases above 3% occur in one-key
scalar eviction's interpreter elapsed/CPU (1.032/1.038), small long-key JIT
elapsed (1.034), small custom-key interpreter CPU (1.031), and the one-key
uncached workload's interpreter work (1.041). Their repeated corresponding
ratios are 0.974/0.987, 0.989, 0.984, and 1.029. The uncached workload's JIT
work costs 3.3% on repetition, although elapsed and CPU remain within 0.7%.
No other repeated focused workload or process cost exceeds 3%.

All six ordinary populations and all eight typed populations are repeated
for eleven cycles. Ordinary work stays within 2.5% of the base, process
metrics within 1.6%, and RSS within 0.9%. The initial 10,000-empty-wrapper
JIT work cost of 3.2% becomes 0.4%. The existing population probe's historical
`fallback` mode still uses its unchanged tuple argument, which now takes
native recency.

Populated typed wrappers retain allocation and construction costs:

| Typed population | Count | JIT work | Interpreter work | JIT RSS | Interpreter RSS |
| --- | ---: | ---: | ---: | ---: | ---: |
| Scalar | 1,000 | 1.037 | 1.030 | 1.010 | 1.007 |
| Scalar | 10,000 | 1.027 | 1.023 | 1.038 | 1.038 |
| Tuple | 1,000 | 1.014 | 1.036 | 1.001 | 1.016 |
| Tuple | 10,000 | 1.024 | 1.027 | 1.027 | 1.026 |

Initial 10,000-scalar work costs are 4.0%/4.6%, with interpreter RSS 5.0%
higher. Repetition reduces the work costs to 2.7%/2.3% but retains about
3.8% RSS costs in both modes. Small typed tuple interpreter work remains
3.6% slower. Empty and callback-fallback typed populations have no repeated
cost above 3%. No general construction or memory improvement is claimed.

The initial unchanged application suite has these geometric means. Work
excludes the startup-only fixture; process metrics include all 24 fixtures.

| Comparison | Work | Process elapsed | Process CPU | Peak RSS |
| --- | ---: | ---: | ---: | ---: |
| JIT / accepted runtime | 0.9973 | 0.9938 | 0.9969 | 1.0019 |
| Interpreter / accepted runtime | 0.9958 | 0.9905 | 0.9886 | 0.9987 |
| JIT / CPython | 1.0108 | 1.3797 | 1.2408 | 1.5614 |
| Interpreter / CPython | 2.1604 | 1.9809 | 1.8763 | 1.3328 |

Fourteen application controls, including every initial cost above 3% and
retained costs from the first candidate, use eleven-cycle repeats. Selected
JIT means are 0.9961/1.0018/1.0053/1.0109 for work/elapsed/CPU/RSS;
interpreter means are 0.9992/0.9968/0.9959/1.0014. These are not repeated
full-suite means. Initial Fibonacci JIT/interpreter work costs of
6.7%/3.7% and JIT JSON RSS of 4.5% do not persist above 3%.

DeltaBlue interpreter RSS measures 1.091 initially and 1.051 on repetition.
List JIT RSS changes from 1.014 to 1.124; both binaries have broad individual
RSS variation. A separate 21-cycle recheck, including the held candidate,
gives DeltaBlue interpreter RSS 0.996 and list JIT RSS 0.997. File-startup
JIT CPU moves from 1.040 on repetition to 1.019 in that recheck. All three
batches remain archived; no general application speedup or memory reduction
is claimed.

Two 31-cycle batches cover ordinary, no-site, isolated, and import startup.
Initial JIT elapsed ratios are 1.010/1.033/1.006/1.003; repeated ratios are
1.008/1.013/1.012/1.006. The initial no-site CPU cost of 1.082 becomes 1.012;
repeated JIT CPU spans 1.008-1.029, with RSS within 0.7% of the base.
Ordinary JIT startup still takes 1.689 times CPython elapsed. The accepted
base's Windows gate also remains unresolved: cold sumvm/nested-loop/jitloop
ratios are 1.430/1.240/1.212 against the PR merge base, while the separate
warm diagnostic is near parity. This increment is not a fix for that gate.

The held first candidate already improves large typed calls about 5.2 times,
but retains 4-6% costs in several LRU controls, typed-population costs, and
unrelated application/startup costs. The final revision's paired repeated
large typed work is 0.998/0.988 of that candidate; capacity-128 typed work is
0.959/0.970. Large custom-key work is 0.923/0.920. Its assembly has a larger
call handler and stack reservation, so a smaller generated-code footprint
is not the explanation. Both candidate histories remain available.

## Provenance

The executable is 51,894,280 bytes, 32 bytes smaller than the accepted base.
Its SHA-256 is
`547bb9e2bb8f941c5fd216674281ff3872d8a17918ca880146da9217fba8ff88`;
the runtime patch SHA-256 is
`5e3d3ef45c62f19377675fe152dcd1ac2620d243835992e800d23055b6c3794e`.
Source snapshots, logs, raw samples, and summaries remain under
`target/performance/lru-typed-native-v2-investigation/`, with the executable
at `target/performance/weavepy-lru-typed-native-v2`. The held first candidate
and its measurements remain in the sibling `lru-typed-native-investigation/`
directory. Application and initial startup reports use the sibling
`lru-typed-native-v2-rb102e92-` prefix.
