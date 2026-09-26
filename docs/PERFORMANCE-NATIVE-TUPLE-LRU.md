# Bounded LRU recency for native tuple keys

Untyped caches now use dense index links for exact tuples containing exact
integers, long integers, strings, and nested tuples of those leaves. This
extends the preceding scalar optimization to ordinary multiargument and
keyword calls. Previously, every bounded tuple-key hit shifted entries
through an ordered dictionary, making its cost grow with cache capacity.

Admission visits at most 64 objects and eight tuple levels per key.
Larger or deeper keys remain valid through the existing fallback. Typed
caches and keys containing subclasses or callback-capable objects also
retain that path. Successful tuple hashing still happens before the cache
borrow. Both incoming and matched stored keys are checked; a native-value
comparison can't hide a subclass owner inside a privately replaced key.

The existing cache lock protects tuple entries and recency together,
including in GIL-disabled mode. Demotion restores logical recency before
callback-capable comparisons. Hits retain the stored tuple owner. Recursive
insertion, clear during a wrapped call, keyword order, exported recency
storage, and value finalizers preserve the tested Python behavior. No
wrapper field or global object-layout variant is added.

## Validation

Built from `b1c903d`, the change passes all 408 VM tests, embedding's explicit
1 MiB stack test, VM Clippy with the established exclusions, and compilation
without default features. The native tuple regression covers six key forms
and five capacities against an independent recency oracle, equal but
distinct keys, colliding native hashes, bounded admission, subclasses,
private stored-owner guards, recursion, demotion, hashing errors, and
finalizer frames. Existing scalar fixture workloads and assertions remain
unchanged; comments now distinguish native tuple transitions from fallback.

The frozen CLI passes 38 regression runs, five extra GIL-disabled shared
counter/concurrent-clear runs, 468 cache matrix checks, and 36 cache
population checks. All 24 selected upstream call, descriptor, GC, and LRU
groups pass across three modes, without ignored exceptions. The complete
31-test `TestLRUC` group now passes GIL-disabled execution, including its
previously failing tuple-key threaded test. Twenty additional GIL-disabled
runs of all three upstream threaded LRU tests pass, sixty checks total.
Compilation decisions match the preceding binary in 53 probes and three
applications.

The unbounded and callback-capable map paths are unchanged; their separate
concurrency limitations remain. This increment fixes the reproduced lost
entries for admitted bounded tuple keys, not every concurrent cache case.

## Measurements

Comparisons use accepted `b1c903d`, optimized CPython 3.14.5 with PGO, LTO,
and the tail-call interpreter, and Intel macOS. Paired process order
alternates, warmup is discarded, and setup, checks, and normal collection
remain timed. All owned builds, correctness checks, and diagnostics finish
before timing. Codex renderers and other desktop processes remain active;
this isn't a dedicated idle host. No workload, baseline, gate, or JIT
budget is changed.

The unchanged 31-case cache matrix uses seven cycles; nineteen selected
cases use eleven-cycle repeats. Ratios below are repeated candidate work
divided by `b1c903d` work, with lower values better.

| Key form and capacity | JIT | Interpreter | JIT / CPython |
| --- | ---: | ---: | ---: |
| Tuple, 128 | 0.773 | 0.763 | 6.066 |
| Tuple, 4,096 | 0.167 | 0.169 | 5.368 |
| Keyword, 128 | 0.791 | 0.779 | 5.861 |
| Keyword, 4,096 | 0.205 | 0.204 | 5.384 |
| Transition, 4,096 | 0.156 | 0.159 | 6.169 |
| Typed, 128 | 1.010 | 1.010 | 9.997 |
| Typed, 4,096 | 0.975 | 0.976 | 35.181 |

Initial large tuple, keyword, and transition work ratios are
0.171/0.168, 0.208/0.205, and 0.159/0.156 in JIT/interpreter modes.
On repetition, their JIT process elapsed ratios are 0.399, 0.430, and
0.404; CPU ratios are 0.362, 0.390, and 0.371. Peak RSS ratios are 1.020,
1.015, and 1.009. The transition workload still makes the same tuple call;
that call now stays native. Typed keys retain the shifting path and a large
CPython gap.

Costs remain. Initial work increases above 3% occur in size-one eviction
(interpreter 1.067), size-one cycling (JIT 1.052), hot capacity-128 calls
(1.035/1.038), size-one unbounded calls (1.092/1.069), and large string-key
calls (interpreter 1.032). Repeated work costs above 3% are:

| Control | Mode | Work ratio |
| --- | --- | ---: |
| Scalar eviction, 4,096 | JIT | 1.037 |
| Scalar cycling, 1 | JIT | 1.031 |
| Scalar cycling, 128 | Interpreter | 1.031 |
| Scalar cycling, 4,096 | JIT | 1.035 |
| Hot calls, 128 | Interpreter | 1.030 |
| Unbounded calls, 1 | Interpreter | 1.035 |

Custom-key capacity-128 JIT process CPU is 1.039 on repetition. No other
repeated focused process or RSS cost exceeds 3%. The paired samples vary:
large scalar cycling ranges from 0.976 to 1.062, with nine of eleven
pairs slower; size-one unbounded interpreter work ranges from 0.962 to
1.089, with ten pairs slower. These costs aren't discarded. A static
inspection finds unchanged stack reservation in the cache operation and
call handler, and an address-normalized identical call handler. It doesn't
establish the cause, and no speculative outlining change is included.

Six cache-population cases use seven cycles, with all three 10,000-wrapper
cases repeated for eleven cycles. No initial population cost exceeds 3%.
The tuple-key population's initial JIT RSS ratio of 1.029 becomes 1.018
on repetition, while repeated work is 0.993/0.989. Empty and scalar
populations stay near baseline. The probe's historical `fallback` name is
retained; its unchanged tuple argument now uses native recency.

The unchanged 24-fixture application suite uses three cycles. Its geometric
means are below; workload means exclude the startup-only fixture.

| Comparison | Work | Process elapsed | Process CPU | Peak RSS |
| --- | ---: | ---: | ---: | ---: |
| JIT / preceding runtime | 0.9979 | 0.9841 | 0.9871 | 1.0050 |
| Interpreter / preceding runtime | 0.9959 | 0.9842 | 0.9871 | 0.9965 |
| JIT / CPython | 1.0169 | 1.3611 | 1.2257 | 1.5696 |
| Interpreter / CPython | 2.1693 | 1.9641 | 1.8627 | 1.3262 |

Sixteen fixtures covering every initial movement above 3%, plus controls,
are repeated for eleven cycles. None retains a cost above 3%, including
initial Richards/call/generator work, spectral-norm CPU, JSON RSS, and
file-based startup CPU costs. The selected-repeat JIT means are
0.9959/0.9818/0.9848/1.0034 for work/elapsed/CPU/RSS; interpreter means are
0.9968/0.9831/0.9841/0.9973. These aren't repeated full-suite means, and
no general application speedup is claimed.

Two separate 31-cycle startup batches retain near-baseline JIT elapsed.
Ordinary/no-site/isolated/import elapsed ratios are initially
0.991/0.996/0.998/0.997 and repeatedly 0.998/0.991/0.993/0.998.
The initial no-site CPU cost of 1.033 becomes 0.998; repeated JIT CPU
ratios range from 0.992 to 1.001. Repeated interpreter elapsed ranges from
0.950 to 0.990 and CPU from 0.922 to 0.998. Ordinary JIT startup still
takes 1.658 times CPython elapsed. No general startup or memory improvement
is claimed.

## Retained evidence

The executable is 51,894,312 bytes, 176 bytes larger than its predecessor.
Its SHA-256 is
`6b15756a84d1131c3c87cccdf7549956f4730f074235c036d9c32acc1cb516db`;
the runtime patch SHA-256 is
`a59cd133fbab14591604625a5e3438aec6af0f16f8018c9f9e6115a5a0dc0fea`.
Sources, logs, and measurements remain under
`target/performance/lru-native-tuples-investigation/`, with the frozen
binary at `target/performance/weavepy-lru-native-tuples`.
