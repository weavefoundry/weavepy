# Shared weakref keys: allocation evidence and broad controls

The checkpoint release retains the shared weakref keys. Exact live-allocation
profiles support the focused memory reduction; the complete workload suite is
roughly neutral against the preceding release. The no-site startup control is
slower. No claim of universal performance improvement is warranted.

The source is commit `566c7bf2a281a97e2537ab19db371f124317be53`. The measured
candidate SHA256 is `8899e3679267f571bc5d50167197c8a558e19c5f3278bf3e74df0a8607213284`.
The preceding release is `36989234` (borrowed collector handles); the comparison
base is `b9e58099` (collector traversal lists). Full identities are in
[environment.json](environment.json) and [suite.json](suite.json).

## Live allocations

All eight new instrumented captures passed their heap readiness checks. Both
3,000-wrapper populations, with and without callbacks, add 60,000 live
allocations at the baseline factory's verified direct malloc sites and 39,000
at the candidate's sites relative to their zero-node controls. The baseline's
seven key sites account for the 21,000 removed allocations and 624,000 requested
bytes before allocator rounding. The candidate's shared table has seven live
allocations in every profile, with no population growth. Ordinary and
finalizable object controls add zero allocations at these selected sites.

[Allocation totals](allocations.json) retain all controls, per-site counts,
instrumented bytes, requested-byte inferences, and raw history hashes. The
complete local stack groups and captures remain under `target`; they aren't
included in Git. Instrumented block sizes differ from ordinary malloc sizes.
These are selected live allocations, not allocation churn or OS RSS.

## Startup and complete workload suite

All nine startup/import cases completed 31 paired cycles, and all 24 workload
cases completed five paired cycles. The per-binary frozen caches stayed
unchanged. Every measured sample is retained; none was excluded or repeated.
The gate admitted the run at load at most four. During measurement, one-, five-,
and fifteen-minute load ranged 2.662-5.029, 3.479-3.946, and 3.707-3.842 on eight
logical CPUs. Swap usage changed from 1,105.69 to 1,097.69 MiB. These host
conditions limit interpretation of small differences.

Candidate/previous geometric workload-time ratios are 1.00121 with JIT and
1.00088 interpreted. Peak-RSS ratios are 1.00020 and 1.00120. No-site startup
wall time is 1.01845, CPU time 1.01369, and peak RSS 1.00847 relative to the
preceding release. All nine startup wall-time ratios are slightly above one;
most import RSS ratios are slightly below one. The binary is 1,024 bytes larger.
Consult the raw startup report for every control and metric.

The three-release comparison also isolates the borrowed collector handles:
geometric workload-time ratios versus the earlier base are 0.99592 with JIT
and 0.99550 interpreted, while peak-RSS ratios are 1.00151 and 1.00242. These
broad controls do not replace the pending focused collector study.

Against CPython 3.14.7, the candidate wins 6 of 23 workload timings with JIT and
0 of 24 peak-RSS comparisons. JIT geometric workload time is 3.45420 times
CPython's, and peak RSS is 2.07864 times its. Interpreted ratios are 9.41923 and
1.90937. Pickle remains 219.37 times CPython's workload time; datetime remains
121.91 times. [All ratios](summary.json), [all workload samples](suite.json),
and [all startup samples](startup.json) include unfavorable results.

The user's performance goal remains unachieved. Energy, controlled build time,
parallel scaling, and comparisons to free-threaded CPython weren't measured.
