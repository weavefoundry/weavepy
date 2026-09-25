# Keep deferred-instance calls independent of filter collisions

The cached-predicate tests can execute correctly while never reaching the intended
fast path. On `949d5df`, both Windows and macOS CI record successful classification,
warmup, call-slot matching, and argument-binding checks, followed by zero calls
passing the instance-release guard. Direct evaluation of the cached predicate
succeeds.

A local repeated-process diagnostic reproduces the rejected state: an instance
has two strong owners, its tracking is still deferred, and the exact collector
registry says it is untracked. The collector's probabilistic filter nevertheless
reports a possible match. The general drop grader conservatively marks that case,
and the pure-call guard rejects the otherwise safe release. Allocator placement
can therefore decide whether a small function uses the optimization.

`core_droppable` now accepts a shared instance whose existing deferred-tracking
flag is set. The flag proves that neither the collector nor the weakref registry
holds an owner. The required strong count greater than one still rejects the
final-owner release. Non-atomic stores, weakref creation, explicit tracking queries,
and other tracking operations revoke the flag before publishing a collector owner.
Tracked instances retain the existing grader. The change adds no registry lookup,
metadata, retained owner, allocator adjustment, or JIT admission.

The general drop grader and collector remain unchanged. Existing predicate and
slot coverage assertions still require more than 1,000 interpreter-path hits;
semantic checks still run with the JIT both enabled and disabled.

## Validation

A new isolated Rust regression models stale filter bits at a live instance's
address. It verifies that the exact registry is empty and two program owners
remain. The old guard fails this test; the new guard passes. The same test verifies
that the final owner is rejected and that a non-atomic store revokes the proof,
leaving an operand plus collector handle that still requires prompt cleanup.

The new Python fixture passes CPython 3.14.5 and accepted `5066f29` in JIT,
interpreter-only, and GIL-disabled modes before the change. It checks aliases,
explicit tracking, weakrefs, non-atomic stores, cycles, and temporary finalizers.
All 388 VM tests, the embedding lifecycle, VM Clippy with the two established
exclusions, no-default compilation, scoped formatting, and 14 benchmark-tool tests
pass locally. Twenty separate processes pass the original predicate/slot coverage
assertions. Unit CI passes on Linux, macOS, and Windows at `6ea3e53`. Both macOS and
Windows record 23,728 slot-predicate hits and 1,822 DeltaBlue predicate hits in
interpreter mode; their embedding lifecycle also passes.

The frozen release passes 288 regression runs across 96 fixtures in all three
modes, 608 probe checks, and nine selected CPython fixtures. Complete deque results,
operand ordering, temporary-owner cleanup, and the existing writable-caller-local
checks pass. Compilation decisions and JIT statistics match the baseline in all
22 traced probes. The separate native caller-local gap remains unresolved.

## Measurements

The clean `6ea3e53223b98129b7e0452c7979a964ee602d50` release completed in 7 minutes,
46 seconds; the build controller closed before freezing. Its executable is
51,877,200 bytes, 4,096 bytes larger than accepted `5066f29`. SHA-256:
`c30493d1a9c0f84b2dfa0786c633a4ccaa7aee1680320ad30e2a24a0ff684423`.
Measurements use the existing macOS x86-64/optimized CPython 3.14.5 methodology,
with isolated timing batches, alternating process order, discarded warmup,
verified frozen caches, and unchanged work sizes. Ratios below are new/baseline;
lower is better. Setup and checks remain timed; process metrics include launch.

| Comparison | JIT work | Interpreter work | JIT peak RSS |
| --- | ---: | ---: | ---: |
| DeltaBlue, seven cycles | 1.008 | 1.002 | 1.018 |
| Richards, seven cycles | 1.005 | 0.950 | 0.987 |
| Call overhead, seven cycles | 0.961 | 0.982 | 0.997 |
| Attribute access, seven cycles | 1.003 | 1.005 | 0.994 |
| Slot arithmetic, one million operations, five cycles | 1.032 | 1.036 | 0.990 |
| Dictionary arithmetic, same work | 1.027 | 1.027 | 0.998 |
| Slot chain, same work | 1.033 | 1.037 | 0.995 |
| Dictionary chain, same work | 1.028 | 1.037 | 0.995 |
| Slot arithmetic, seven-cycle recheck | 1.036 | 1.027 | 0.999 |
| Dictionary arithmetic, seven-cycle recheck | 1.018 | 1.028 | 0.995 |
| Slot chain, seven-cycle recheck | 1.019 | 1.012 | 0.994 |
| Dictionary chain, seven-cycle recheck | 1.013 | 1.038 | 1.002 |

The repeated arithmetic costs remain open. Sustained class-field predicates cost
1.5% JIT and 3.3% interpreter; integer, slot, float, string, and mixed predicates
are closer to baseline or improve. Several conditional selector probes improve
2-4%, but scalar-return cases retain small costs. The held conditional-getter
shortcut is absent from this runtime.

Across the unchanged 24-fixture suite (three cycles), JIT geometric means are
0.991 workload, 0.986 process elapsed, 0.986 CPU, and 1.006 peak RSS. Interpreter
means are 0.993, 0.979, 0.976, and 0.993. The workload mean excludes startup.
Against CPython, JIT means remain 1.080 workload, 1.401 elapsed, 1.292 CPU, and
1.568 RSS. This is a modest aggregate improvement against the paired WeavePy
baseline, with substantial CPython gaps and individual regressions still present.

Broad outliers and their seven-cycle rechecks are retained together. AES JIT work
changes from 0.930 to 1.033, so its initial gain isn't established; interpreter
work changes from 1.052 to 1.009. Float-math and spectral-norm JIT costs decrease
from 3.7% each to 1.7%/2.0%; n-body from 2.6% to 0.5%; deque from 2.3% to 0.7%.
At five times the standard call-overhead work, JIT is 0.989 and interpreter 1.002,
with JIT elapsed/CPU 0.980/0.981. The initial 4% call gain doesn't persist at that
larger size.

The 31-cycle DeltaBlue/list/pidigits recheck gives JIT RSS ratios
1.044/0.995/0.973, following initial ratios 1.062/1.085/1.049. DeltaBlue's memory
cost persists; its work ratio is 1.004, so no application speedup is established.
The source adds no owners or metadata, but that alone doesn't explain away the
measured memory increase. Original suite means and samples remain unchanged.

All four startup cases use 31 cycles. Ordinary/no-site/isolated/import JIT elapsed
ratios are 0.999/0.998/0.998/0.994, CPU 0.995/0.993/1.004/0.992, and RSS
1.000/1.004/1.000/1.000. Interpreter startup elapsed ratios are
0.967/0.948/0.967/0.983. Ordinary startup and imports still take about 1.64 and
2.56 times CPython's process elapsed time.

Source snapshots, immutable binaries, controllers, logs, hashes, and raw results
remain under `target/performance/deferred-instance-drop-investigation/` and
`target/performance/deferred-instance-drop-*`. Original platform failures remain
under `target/performance/embedding-stack-bounds-investigation/`. Platform benchmark
gates were still running at the last inspection; passing unit CI doesn't establish
performance-gate success.
