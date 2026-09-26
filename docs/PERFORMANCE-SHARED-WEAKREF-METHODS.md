# Shared methods for ordinary weak references

Ordinary native weak references now use their existing type-level call and
representation methods instead of allocating two duplicate captured closures
in every wrapper. Saved explicit methods own their wrapper and preserve its
callback. Explicit representation agrees with implicit representation; the old
per-instance method returned a placeholder.

The shortcut requires identity with the constructing thread's native ref type.
Proxies, subclasses, and other class overrides retain their existing storage.
Callback storage, target ownership, collection scheduling, object layouts, and
JIT admission are unchanged. Earlier native-storage experiments remain held;
this change adds no native payload or global object variant.

## Validation

All 409 VM tests pass, along with embedding's explicit 1 MiB stack case,
34 C API unit tests, and the new direct C API weakref integration. VM and
focused C API Clippy, no-default compilation, scoped formatting, and the
repository artifact check pass. Independent saved-call and saved-repr owner
oracles pass CPython and fail the accepted binary in all three WeavePy modes.

The frozen CLI passes 66 regression runs, five additional GIL-disabled runs,
eight observer runs, and 760 probe checks. All 30 selected upstream groups
pass, including full weakref, weak-set, GC, and LRU groups in JIT, interpreter,
and GIL-disabled modes. Twelve native SQLAlchemy/Alembic checks pass; those
extensions can reenable the GIL. Compilation decisions match in 53 probes,
three applications, and nine ordinary populations. Selected validation logs
contain no ignored exceptions.

## Measurements

Comparisons use accepted `5f5fed3`, optimized CPython 3.14.5 with PGO, LTO,
and the tail-call interpreter, and Intel macOS. Process order alternates,
warmup is discarded, and setup, result checks, and normal collection remain
timed. Owned builds, tests, and profiles finish before timing. Desktop load
is recorded, including substantial Spotlight and media-analysis activity;
this is not a dedicated idle machine. No workloads, gates, thresholds, or
JIT budgets change.

The initial matrix covers 38 focused cases at seven cycles and the unchanged
24-application suite at three cycles. Eleven-cycle repeats cover 34 focused
cases and 21 applications, including every initial cost above 3% and relevant
controls. Two startup batches use 31 cycles each. Supplemental rechecks retain
all earlier samples rather than replacing them.

The large-population results below use eleven-cycle repeats. Ratios are
candidate/base; lower is better.

| Case, 100,000 objects | JIT work | Interpreter work | JIT CPU | JIT peak RSS | JIT / CPython work |
| --- | ---: | ---: | ---: | ---: | ---: |
| Weakrefs | 0.919 | 0.911 | 0.922 | 0.901 | 20.51 |
| Callbacks | 0.901 | 0.890 | 0.901 | 0.881 | 33.20 |
| Cycles | 0.915 | 0.907 | 0.917 | 0.913 | 15.93 |
| Finalizers | 0.918 | 0.941 | 0.921 | 0.900 | 17.56 |
| Frozen cycles | 0.926 | 0.921 | 0.922 | 0.911 | 15.74 |
| Weak-set cleanup | 0.905 | 0.899 | 0.905 | 0.873 | 36.49 |

Initial large GC work ratios span 0.916-0.939 in JIT mode and 0.889-0.925
in interpreter mode. Initial weak-set work is 0.889/0.899. Repeated large
weakref elapsed is 0.928/0.915, and callback elapsed is 0.903/0.898.
Repeated 10,000-object populations also improve: JIT work is 0.869-0.930,
interpreter work 0.910-0.960, and RSS is lower in every mode.

There are method-call costs. Repeated explicit `reference.__call__()` work
is 1.324/1.324, with process CPU 1.207/1.263. Saved-call work is 1.110/1.086.
Both now retain the wrapper owner correctly. Common implicit calls remain
near the base. Explicit/saved repr takes 2.09-2.34 times the old work, but now
performs the full representation instead of returning a placeholder. Implicit
repr's initial 10.8% JIT cost becomes 0.8%; a later 3.8% interpreter cost
becomes 1.5% in a separate 21-cycle recheck.

Initial ref/proxy access and bound-call costs of roughly 3-7% do not remain
above 3% on repetition. Ordinary populations initially show work/process costs
up to about 10%. Most disappear in the repeat. A new native-integer interpreter
work/CPU cost of 11.2%/11.3% becomes 0.1%/-2.2% in a 21-cycle recheck.
An additional 100,000-object integer-subclass comparison retains 1.0%/2.6%
JIT/interpreter work costs and a 3.3% interpreter RSS cost. Native-string work
is 0.9%/2.9% higher in its recheck. These batches remain archived; no general
ordinary-instance or memory improvement is claimed.

Initial full-suite geometric means follow. Work excludes the startup-only
fixture; process metrics include all 24 fixtures.

| Comparison | Work | Process elapsed | Process CPU | Peak RSS |
| --- | ---: | ---: | ---: | ---: |
| JIT / accepted | 1.0229 | 1.0204 | 1.0245 | 0.9992 |
| Interpreter / accepted | 0.9923 | 1.0018 | 1.0031 | 0.9969 |
| JIT / CPython | 1.0136 | 1.3648 | 1.2256 | 1.5685 |
| Interpreter / CPython | 2.1506 | 1.9731 | 1.8764 | 1.3413 |

The 21 selected application repeats have JIT means 0.9998/1.0024/1.0041/1.0003 for work/elapsed/CPU/RSS, and interpreter means 0.9960/0.9962/0.9951/0.9993. These are not repeated full-suite means.
Initial JIT work increases reach 13.0% in nested loops; those and the other
initial application costs do not persist above 3%. List JIT RSS changes from
1.145 initially to 1.008 on repetition. DeltaBlue interpreter RSS changes
from 1.001 to 1.048; a separate 21-cycle recheck gives 0.974.
All three batches are retained.

Initial ordinary/no-site/isolated/import JIT startup elapsed ratios are
0.970/1.002/0.974/1.017; repeats give 1.013/1.025/1.004/1.009. The initial
no-site CPU cost of 9.0% becomes 1.5%; other repeated JIT CPU costs are at
most 2.4%. Ordinary JIT startup still takes 1.697 times CPython elapsed.

CPython parity is not achieved. The accepted base's Windows gate also remains
unresolved: sumvm is 1.204 times the PR merge base after retry, while the
separate cold/warm diagnostic is 1.195/1.002. This increment does not claim
to fix cold compilation or establish universal compatibility.

## Provenance

The executable is 51,894,280 bytes, unchanged from the accepted binary.
Its SHA-256 is
`ba7f79ba4f96b88333eb6c515036120ce7890d3873ba9953d37e990ce62b0e55`;
the runtime patch SHA-256 is
`2b27f2fdbcca9159199d9b90b232e67e23d7878d15d2c678d306a41d9f7bde9e`.
Source snapshots, profiles, logs, and all measurement batches remain under
`target/performance/weakref-shared-methods-v1-investigation/`; the executable
is `target/performance/weavepy-weakref-shared-methods-v1`. Initial application
and startup results use the sibling `weakref-shared-methods-r5f5fed3-` prefix.
