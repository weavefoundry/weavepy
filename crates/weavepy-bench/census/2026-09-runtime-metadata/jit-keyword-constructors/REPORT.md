# Keyword constructor replanning

This experimental candidate lets the JIT reconsider a global constructor or builtin when a keyword call cannot use the statically resolved Python-function calling convention. The global becomes an identity-guarded object, and the existing dynamic call path performs keyword binding. Original argument order, error behavior, callable capture, and callback counts remain covered by semantic checks.

The production change is confined to `weavepy-jit/src/analyze.rs`. Three compiler tests and two VM tests cover the new path; no production VM helper or calling convention changes. The candidate is based on the validated but unmeasured exact-indexing candidate. It has not replaced the primary CLI.

The CLI SHA-256 is `ae0f04e61f51c23700c52c352009651ba079dc671c9f0afe79f9a0ae649cc943`; its size is 44,098,272 bytes, unchanged from its immediate predecessor. Build duration was not controlled and is not a performance result.

## Correctness and coverage

The current source version passes 74 JIT tests, 357 VM tests, 156 C API tests, strict lint checks, and the no-JIT build check. All 16 new semantic cases match CPython across the compared configurations. The prior local-state, indexing, and formatting controls pass their recorded expectations. The existing staticmethod class-subscription difference remains explicit in the indexing oracle.

Targeted runtime validation passes all 99 checks and all 44 recorded compilation paths. Broader compatibility and census validation are pending at this report's initial preparation; later results must be appended before making a performance decision.

Three focused drivers now compile: keyword construction, keyword construction using defaults, and keyword deque construction. The full deque census gets past its keyword-call rejection but still rejects with `MixedArithTypes`, so its hot loop remains interpreted. The reported entry PC is not the location of the offending arithmetic instruction.

## Preserved failed attempts

The first offline compiler check stopped before tests because a locked dependency was missing. Fetching the existing locked dependencies resolved it without changing Cargo.lock. The first native suite passed 354 of 357 VM tests. Two new tests checked the wrong native-entry counter; one also used string arguments that hit an unrelated pinned-argument restriction. Those test harnesses were corrected without changing the production patch.

The third initial failure was the existing cross-thread JIT polling test, reporting `SET_FUNCTION_ATTRIBUTE on a shared function`. It passed in an isolated diagnostic and in the subsequent complete suite. Its cause remains unresolved; the initial failure and all diagnostic logs are retained.

Missing regenerable bytecode files from earlier experiment inputs were restored byte-for-byte from the previously verified archive. Earlier frozen manifests now verify again. This candidate's new timing manifest excludes those input bytecode caches while retaining source identities and per-runtime cache stabilization checks.

## Measurement protocol

The planned comparisons cover nine cold and nine warm construction/control cases with seven samples, nine startup/import cases with 31 samples, and all 24 census cases with five samples. Each stage holds the exclusive workload lease and must qualify under the existing host-load gate. The immediate reference is exact indexing; the retained reference is thin value storage. Every sample and any failed gate remains preserved.

All ratios use candidate/reference, with lower values better. Workload time (23 cases), whole-process metrics (24 cases), and the historical 21-case cohort must remain separately labeled. No universal performance claim is supported.

## Completed runtime validation

All 275 compatibility cases passed. The complete 24-case census audit matched CPython's results and retained every native trace and path counter. Codegen inspection found no change to the recorded native helper stack reservations relative to the immediate predecessor; that is a static codegen observation, not a peak-memory measurement. Both runtime and timing readiness verifiers passed against the frozen manifests.

## Recorded performance and decision

Three planned stages completed with all samples retained. The final full-suite comparison against thin value storage did not start: its load gate expired after 600 seconds. No child or output dataset exists for that stage. The complete-record verifier explicitly distinguishes these outcomes; the original four-stage completion verifier remains unchanged.

Against the immediate predecessor, the nine-case focused geometric mean rose by 3.98% for cold workload time and 3.06% for warm workload time. The newly compiled keyword constructors became 13% to 17% slower while their peak RSS fell about 1.5%. Keyword deque construction became about 5% to 7% slower. All seven paired constructor timings lost. The retained-reference focused comparison also showed constructor regressions; every result remains in the summaries.

Across all 23 workload cases, time/predecessor was 1.004782 and time/CPython was 3.406959, with six CPython wins and 12 predecessor regressions. Across 24 process cases, wall time/predecessor was 1.005266, CPU time/predecessor was 1.003910, and peak RSS/predecessor was 1.001170. Peak RSS/CPython was 2.026158, with no CPython memory wins. The historical 21-case cohort was 2.099164 times CPython using paired medians, or 2.094315 using ratios of medians. These cohorts and aggregation methods must not be mixed.

This candidate is preserved as an unsuccessful experiment and has not replaced the primary CLI. Compilation coverage improved, but performance did not. The focused traces show 64 additional caller exits in each of the two constructor cases. A follow-up will test whether the dynamic native-call relay's instance-only object-return packer causes those exits; that explanation is not yet confirmed by an experiment.
