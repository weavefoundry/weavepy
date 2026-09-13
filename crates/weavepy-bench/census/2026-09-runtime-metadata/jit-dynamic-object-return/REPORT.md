# Dynamic native object returns

This experimental candidate fixes a native-call result conversion that caused compiled constructor callers to retire after repeated exits. A dynamic callee can return an object-tagged integer or another builtin value, but the native relay used an older packer that accepted only instances and None. The dynamic interpreter path already accepted arbitrary objects.

Only dynamic native calls now select arbitrary object-result pinning. Typed call sites retain their existing result checks. Caller guards are still revalidated before accepting the result; pin-capacity failure still parks the completed value and exits after the call. No completed call is replayed. The public JIT ABI and scalar-return paths are unchanged. Production changes are confined to tier2.rs; three regression tests are added in lib.rs.

The isolated CLI SHA-256 is `9c7b88231779913f62626cdd826646c24d2ab0d7f2b1d56080e3274f25b3a659`. Its size is 44,098,272 bytes, unchanged from the immediate keyword-constructor reference. It has not replaced the primary CLI.

## Reproduction and native validation

The initial integer regression test returned the correct 65024 checksum but failed with 64 native exits, versus the required zero. Production code was unchanged for that reproduction. The identical test passed with zero exits after the scoped fix.

The final test set also covers ten result types and their identities, requiring hot-loop entry and at least 100 native calls for each payload. A callback test changes a guarded global during construction and checks both the required caller exit and exactly-once event counts. All three tests pass.

All 74 JIT tests, 360 VM tests, 156 C API tests, formatting, strict all-target lint, and the no-JIT check pass. These checks used an isolated unoptimized test profile with Rust debug assertions enabled and debug symbols disabled. They do not establish build-performance improvements.

The broad-type test required harness corrections, all preserved. Its first list-building consumer compiled but did not enter through OSR. A later version wrongly required the initializer to compile for list payloads, although the relevant caller and callee compiled. Adding a direct native-call assertion exposed an unused argument that prevented native entry. The final source explicitly uses the integer argument and checks native calls. Production code did not change during those harness refinements. Failed sources, logs, and diagnostic traces remain available.

## Release checks and coverage

The optimized CLI passed the inherited 16 keyword semantic cases, 14 local-state cases, 34 indexing cases, nine indexing controls, nine constructor controls, and the formatting control against their recorded expectations. The preexisting staticmethod class-subscription difference remains explicit. All 99 targeted release-runtime checks and 44 recorded compilation paths pass.

In each full constructor probe, native-to-native calls rose from 116 to 40,378. The 64 retirement-causing caller exits disappeared; eight pin-pressure exits occurred instead. Result checksums are unchanged. This confirms the return-path fix in the optimized build, but speed and peak-memory effects are still unmeasured. The full deque benchmark retains its mixed-arithmetic compilation barrier.

Full compatibility validation, static codegen inspection, the complete census audit, and timing remain pending at this report's preparation. Later outcomes must be recorded before a performance decision.

## Preserved preparation failures

The first release-cache seed stopped at its free-space assertion before creating a destination. After complete native validation, cargo clean removed only the isolated experiment's regenerable test target: 4,611 files, 2.2 GiB. Sources, logs, traces, and earlier runtime identities verified before and after cleanup. A fresh seed driver used the unchanged copy-on-write method successfully.

The first CLI build preparation had a path typo introduced by a text substitution. It stopped before creating runtime output or compiling. The corrected version built successfully. All failed preparation scripts and logs remain. The observed six-minute-thirteen-second release build is not a controlled build-time result.

## Measurement plan

The 204 runtime method/source identities and 122 timing identities are frozen. Four serial comparisons are planned against the immediate keyword-constructor build and the retained thin-value-storage reference. Each uses the established load gate and exclusive workload lease. The focused comparison preserves nine cold and nine warm constructor/control cases with seven samples; the full comparison preserves nine startup/import cases with 31 samples and 24 census cases with five samples.

Report every gain and loss in workload time, process wall and CPU time, peak RSS, startup/imports, and binary size. Keep 23-workload, 24-process, and historical 21-case cohorts separate. A gate that collects no samples supplies no performance evidence. The earlier keyword-constructor experiment remains unsuccessful; this fix is not yet measured or promoted.

## Completed release readiness

All 275 compatibility cases passed. The complete 24-case census audit matched CPython's results and preserved every native trace. Recorded native helper stack reservations are unchanged from the immediate reference. Runtime and timing readiness verified against the frozen 204 check identities and 122 timing identities. The candidate is ready for its planned controlled comparisons; no timing result is implied by these checks.


## Timing gate and decision

The first focused/immediate load gate expired after 600 seconds without launching a child or creating a dataset. Its session closed with exit 75 and its report is explicitly not_started. No timing sample exists for this candidate, and the other three stages have not been launched. Do not infer speed or peak-RSS gains from the zero-exit test or compilation counters.

Preserve this as a validated, unmeasured native-return fix. It has not replaced the primary CLI. The next bounded investigation is the avoidable pinning of object-tagged integers when the dynamic caller already requires an exact integer. Complete traces confirm eight pin-pressure exits in the constructor probes, providing a concrete target for that follow-up.
