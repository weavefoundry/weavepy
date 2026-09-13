# Object indexing: measured experimental candidate

The new object-lane indexing path reduces warm workload time by 27-43 percent in five integer-indexing cases. It does not establish an overall performance win. The full suite remains 3.39 times CPython workload time and 2.02 times its peak RSS. The candidate and complete evidence are preserved for further work; the primary phase 79 checkout and CLI remain unchanged.

## Implementation

The isolated source 03 build adds ObjGetItemInt for an object receiver and an exact machine-integer index. It calls the interpreter's complete subscription protocol, publishes the lookup PC, supplies the native activation shell, and revalidates guards after callbacks. Exact integer results return unboxed without a temporary result pin. Other results are parked and resume after the already-completed lookup. Exceptions raise at the lookup instruction; a rejected receiver pin resumes before any lookup. Negative indices and exact result identity are preserved. Existing list, dictionary, string, and bytes paths remain.

The ten-file patch is relative to the validated phase 84 local-writeback sources, not directly to Git HEAD. sources.tar.gz contains all 340 files used by the build. The CLI SHA-256 is 8eaa743234cd2c3b29e9372d8d3fb3f4f3947d87c12bab98a545ecc779e6fdd3, with 44,098,272 bytes, 208 more than phase 84. The recorded compiler and native-call stack prologues are unchanged. These static sizes are not cumulative stack or RSS measurements.

## Validation

Source 03 passed 71 compiler, 353 VM, and 156 C API tests, formatting, strict compiler/runtime lint, and a no-JIT build check. Runtime checks passed 99 targeted cases, 44 inherited path checks, and 275 compatibility cases. All 14 prior exact local-state cases match CPython in interpreter, JIT, and GIL-disabled execution.

The new 28-case indexing oracle preserves exact result identity, exceptions and traceback bindings, callback counts, global mutation, recursion, caller location, and temporary-result lifetimes. read_item, read_state, and read_offset actually compile. Twenty temporary objects have no surviving weak references. All new outputs match the previous interpreter. Twenty-seven cases match CPython; an existing staticmethod __class_getitem__ mismatch remains in both baseline and candidate: WeavePy supplies an extra class argument. This optimization does not fix it.

All nine focused workloads have exact CPython checksums. The five new integer cases compile where the previous build rejected read_value. Float and Boolean result fallbacks remain explicit. One inherited formatting control now compiles and reconstructs for its Hour subclass values. An initial readiness assertion expected rejection, so that first census launch stopped before any row or timing. A separate CPython/baseline/new/JIT/interpreter/GIL-disabled check confirmed the same two 1220000 checksums. Readiness version 2 changes only this compilation expectation and requires its native fallback trace; all 43 other path expectations remain unchanged. Original failures and both method versions are retained.

The current authoritative 24-fixture audit preserves every original definition and work value, replaces only the timer footer with result output, and retains complete compressed traces. All 24 results match CPython. The deque rejection moves from object indexing to CALL_KW (callee kind); the list rejection moves to MixedArithTypes. This is progress through the compiler, not proof that either whole workload now runs natively. JSON shows additional compilation and deopts in regex-related paths.

## Performance

All four planned stages completed with return code 0, completed load gates, and done stage files. There were no timing retries, discarded samples, or overlapping owned workloads. Each stage ran outside the filesystem sandbox under the exclusive lease, after three ten-second-spaced load observations at most four for both one-minute and five-minute load. Host-load telemetry remains preserved. Every cache remains unchanged after stabilization.

Focused runs use nine cases, seven alternating paired samples each, separately cold and warm. Full runs use the unchanged 24-fixture census with five samples and nine startup/import cases with 31 samples. GIL-disabled execution is a correctness control only. All ratios below are new/reference; lower is better.

| Comparison | Cold workload time, nine cases | Warm workload time, nine cases | Cold peak RSS | Warm peak RSS |
| --- | ---: | ---: | ---: | ---: |
| Immediate phase 84 |0.863026|0.794258|1.002965|1.004150|
| Retained phase 69, cumulative |0.860506|0.804798|1.009113|1.009567|

Against phase 84, all five integer cases win all seven cold and warm workload-time pairs. Their cold ratios are 0.7028-0.8160 and warm ratios 0.5733-0.7297. The cold Boolean fallback ratio is 1.0249, with zero pair wins. List/dictionary and float/Boolean fallback rows are retained, including losses.

| Full-suite comparison |23-workload time |24-process wall time |24-process CPU time |24-process peak RSS |
| --- | ---: | ---: | ---: | ---: |
| Immediate phase 84 |0.993192|1.006412|1.004241|1.002709|
| Retained phase 69, cumulative |1.006649|1.001714|0.999933|1.005724|

Against phase 84,12 of 23 workload times and 18 of 24 peak-RSS rows regress. JSON workload time is 1.037492 of baseline; all five pairs are slower, ranging 1.02433-1.04186. Its RSS ratio is 1.010373. Startup wall-time ratios range 0.998784-1.028716. Against the retained reference, 12 of 23 workload times and all 24 peak-RSS rows regress; startup wall ratios range 1.001477-1.031840. The small aggregate workload change does not erase these tradeoffs.

The final retained-reference run reports 23-workload WeavePy/CPython time 3.394507, with six WeavePy wins, and 24-process RSS 2.018833, with no WeavePy wins. The historical 21 subset is 2.094695 (paired) or 2.090891 (ratio of medians), and must not be mixed with the 23-workload aggregate. Cross-run CPython ratios do not isolate a code change; paired new/reference results do.

## Archive

Six curated files contain this report, the phase 84-relative patch, the complete 340-file source snapshot, raw evidence, the verified manifest, and the final export log. Compiler-test harness versions 01-03, the failed readiness attempt, corrected method versions, all semantic outputs, all native traces, and all four timing datasets remain. Build outputs, executable binaries, frozen caches, and generated standard-library dependency trees are excluded; the latter have full file inventories and hashes. No commit or push was performed.
