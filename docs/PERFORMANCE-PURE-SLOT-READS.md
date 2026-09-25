# Read cached slots in small functions

The pure-leaf evaluator previously read instance-dictionary cache hits directly but
ignored `LoadAttrSlot`. Its generic resolver cannot settle genuine slot descriptors,
so even a warmed slot getter or arithmetic leaf could repeatedly fall back to the
full interpreter. Against `6d6fd89`, the sustained slot-predicate probe takes about
seven times CPython's workload time.

A shared borrowed field reader now handles dictionary and slot cache hits in direct
getters, decoded predicates, and general pure-leaf evaluation. It validates the class
version, rejects native instances, and checks the requested name at the cached index.
Slots retain the ordinary cache's name-lookup fallback when insertion order differs
or deletion changes an index. `peek` rejects shared storage and mutable borrows; live
arguments or scratch values root the receiver until the result is retained. No
Python callback, mutation, or owner release crosses a borrowed reference.

The change adds no persistent metadata or owners. It preserves existing fallback for
descriptors, lookup overrides, stale class versions, missing fields, rich operations,
and observers. It changes no JIT admission, compilation budget, work size, or gate.

## Validation

All 387 VM tests, the unchanged 1 MiB embedding test, VM Clippy with its two existing
exclusions, scoped formatting, no-default-feature compilation, and 14 benchmark-tool
tests pass. JIT sources are unchanged. The new slot fixture passes CPython and both
preceding accepted releases in all three WeavePy modes before the implementation is
changed. It covers single/small/many storage, differing insertion orders, deletion,
inherited slots, descriptor priority, class and lookup-hook mutation, rich callbacks
with caller frames, chained reads, object identity, prompt cleanup, and tracing.

The focused VM test records 31,718 slot getter reads, 23,684 predicate reads, and
31,636 reads through general evaluation. Thus the semantic fixture exercises all
three new routes. Subsequent macOS and Windows CI runs pass the Python assertions
but record zero predicate hits with native compilation enabled. The coverage test
now runs both modes and requires interpreter-path counts with the JIT disabled;
native calls may bypass those counters. Locally, that mode records 31,739 getter,
23,728 predicate, and 31,724 general reads. Both modes retain all semantic assertions.
Windows CI for `c3b2bd4` still records zero predicate hits with the JIT disabled,
while getter coverage succeeds. Native dispatch therefore isn't a complete
explanation. Failure diagnostics now print the getter, predicate, and arithmetic
function shapes, bytecode, and caches without relaxing the coverage requirement.
On `3e1453d`, macOS and Linux unit CI pass; Windows correctly classifies the
predicate but still records zero hits. Test-only call-guard and field-hit counters
now narrow that remaining discrepancy without changing runtime behavior.
The reusable field-read probe pairs slot getters, arithmetic, and
chains with dictionary controls; setup and result checks remain timed.

The frozen release passes 282 regression runs across 94 fixtures in JIT,
interpreter-only, and GIL-disabled modes, plus 448 probe checks. Its binary is
51,873,104 bytes, 160 bytes larger than the preceding accepted binary. Its
SHA-256 is `987746acc5c578443bcd9483433f9068f15d9c08bfba9e94bf147d95a043a255`; baseline `6d6fd89` is
`58e904115755973c50311371967a0c96ca111e863bcacf522fda8ec9770c628d`.
The release build and controller completed before the executable was frozen.
Source snapshots, hashes, raw data, and immutable binaries remain under
`target/performance/pure-slot-reads-investigation/` and
`target/performance/pure-slot-reads-*`.

## Measurements

Measurements use macOS x86-64 and optimized CPython 3.14.5 with the existing paired
methodology. Standard probes use seven cycles and 200,000 operations; sustained
probes use five cycles and one million. Process elapsed, CPU, and RSS include the
complete launch. Separate traces show the same compilation decisions and native-call
counts for all 12 field probes. Ratios below are candidate/baseline unless marked
CPython; lower is better. Process metrics are from the sustained runs.

| Workload | Standard JIT work | Sustained JIT work | Elapsed | CPU | Peak RSS | Sustained JIT/CPython work |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Slot predicate | 0.236 | 0.235 | 0.299 | 0.285 | 1.001 | 1.739 |
| Slot getter | 0.385 | 0.395 | 0.479 | 0.458 | 1.006 | 2.100 |
| Slot arithmetic | 0.412 | 0.402 | 0.468 | 0.454 | 1.012 | 2.392 |
| Slot chain | 0.491 | 0.482 | 0.549 | 0.538 | 0.996 | 2.610 |
| Dictionary getter | 1.025 | 1.009 | 1.012 | 1.012 | 1.003 | 2.103 |
| Dictionary arithmetic | 1.042 | 1.027 | 1.018 | 1.029 | 0.997 | 2.321 |
| Dictionary chain | 1.007 | 1.034 | 1.030 | 1.034 | 1.003 | 2.468 |

Sustained slot predicates take 76.5% less work, slot getters/arithmetic about 60%
less, and slot chains about 52% less. Interpreter-only gains are similar. These
workloads still trail CPython. Dictionary arithmetic retains 2.7% JIT/2.0%
interpreter costs at sustained work, and dictionary chains retain a 3.4% JIT cost.
The corresponding standard costs are 4.2%/5.7% for arithmetic and 0.7% for chains.
Slot-arithmetic and integer-predicate sustained peak RSS each rise about 1.2%.

Other predicate controls are mostly close to baseline at sustained work: integer,
float, class, string, and mixed JIT ratios are 1.005, 1.012, 1.009, 1.010, and
0.995. The initial standard string result is worse, 1.044 JIT and 1.060 interpreter.
Those initial results remain recorded alongside the follow-up checks below.

A fresh DeltaBlue profile has 6,215 active worker samples: 1,561 in leaf_burst
(25.1%), 511 in pure_leaf_eval (8.2%), and 325 in attribute resolution (5.2%). Its
other thread's 6,215 wait samples are excluded. The profile ran after all builds
and checks, its owned process exited before timing, and profile timings are excluded.
The frequent conditional input/output getters remain a measured profiling target;
these sample shares are not predictions of an attainable speedup.

Three cycles of the unchanged 24-fixture suite give default-JIT geometric means
of 0.998 workload time, 0.991 process elapsed, 0.992 CPU, and 1.008 peak RSS.
Interpreter-only means are 0.996, 0.996, 0.995, and 0.996. Workload means exclude
startup. Default-JIT means against CPython are 1.077, 1.381, 1.293, and 1.571.
Overall CPython parity remains unachieved.

DeltaBlue's focused/broad JIT ratios are 0.979/0.970, while interpreter ratios
are 1.006/1.007. Its RSS ratios move from 0.933 to 1.067; the broad JSON/list
RSS ratios are 1.047/1.036. These outliers prompted the longer memory checks below.

Seven-cycle probe rechecks retain a 2.5% JIT/2.4% interpreter cost for standard
dictionary arithmetic. The sustained dictionary-chain JIT ratio falls from
1.034 to 1.006. Standard string-predicate ratios change from 1.044/1.060
(JIT/interpreter) to 0.998/1.029. Original values remain recorded, and these
rechecks do not replace the full-suite means.

Thirty-one startup cycles give default-JIT elapsed ratios of 1.017 ordinary,
1.027 no-site, 1.020 isolated, and 1.008 imports. A separate complete 31-cycle
repeat gives 1.011, 1.039, 1.017, and 1.010, respectively. No-site CPU rises
from 1.024 initially to 1.054 on repeat; isolated CPU rises from 1.017 to 1.027.
Repeated no-site RSS is 1.013. These startup costs remain open.

A separate 31-cycle comparison at the original work sizes gives DeltaBlue/JSON/list
RSS ratios of 1.000/0.994/1.007 and JIT workload ratios of 1.003/1.006/1.009.
DeltaBlue's initial workload improvement therefore does not repeat in the longer
comparison. A subsequent 31-cycle identical-baseline DeltaBlue control gives 1.019
RSS and 1.004 JIT work, confirming measurable variation without a binary change.
These runs do not erase the original outliers or establish a memory improvement.

The latest inspected preceding `6d6fd89` CI head has 15 passing checks, 12 running,
and one queued. Its Linux benchmark gate passes; Windows and macOS gates are still
running. The earlier accepted head retains the Windows cold-compilation regression
recorded in [Cached field predicates](PERFORMANCE-CACHED-FIELD-PREDICATES.md).

## Release-guard follow-up

The `949d5df` macOS and Windows diagnostics identify instance-release rejection
after successful predicate classification and binding. A local reproduction
finds a stale positive collector filter on an untracked instance with two owners.
The [deferred-instance release fix](PERFORMANCE-DEFERRED-INSTANCE-RELEASES.md)
uses the existing exact deferral flag while retaining last-owner and tracked-object
cleanup checks. All three platform unit jobs pass on `6ea3e53`; macOS and Windows
now record 23,728 slot-predicate hits with the JIT disabled. The follow-up report
records its paired performance results and remaining arithmetic/memory costs.
The earlier measurements above remain the baseline for that comparison.
