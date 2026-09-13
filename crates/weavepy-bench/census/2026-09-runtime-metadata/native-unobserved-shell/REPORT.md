# Reuse unobserved native activation shells

Decision: Defer native shell reuse; restore the 6810 predecessor before the next experiment.

Against 6810, the 29-control cold/warm JIT time means are 1.002448481 and 0.983882258. Repeated generic positional and method calls improve, but single-callback and recursive controls regress. Warm compiled-object calls rise 3.75%.
Against 6810, the unchanged 23-workload JIT mean is 1.008074180. The 24-process wall/CPU/RSS means are 1.007975555, 1.006083571, and 1.001829073. Fifteen workload rows and seventeen RSS rows regress. This does not establish an overall improvement.
Against retained bf2, the 23-workload JIT mean is 1.007812191. The 24-process wall/CPU/RSS means are 1.005083330, 1.003716686, and 1.001762326. Ten workload rows and nineteen RSS rows regress. Workload time is 3.389340691 times CPython with six wins, and peak RSS is 2.011336976 times CPython with no wins.
The historical 21-row JIT mean is 2.086257212 times CPython in the retained comparison. Keep this cohort separate from the 23-workload and 24-process means. Independently timed CPython references do not establish a controlled historical speedup.
The retained focused cold/warm means are 0.977090469 and 0.950450214, including inherited formatter and default-binding gains. Compiled-object and recursive warm controls lose every pair, as do inherited callback-formatting and compiled-wide-default controls.
Binary size is unchanged from 6810. Both dynamic-call wrapper stack frames remain 448 bytes. Nested and direct native entry frames each grow by 16 bytes. Fewer shell constructions do not establish a peak-memory or stack improvement.
All 353 VM tests, 156 C API tests, 99 targeted Python checks, and 275 compatibility checks pass. All 29 intended fixture paths and source/method identities are verified. The ownership-check correction and its untimed predecessor are preserved. The known native-frame identity mismatch remains unresolved.
All four timing stages completed sequentially under the exclusive lease. Every launched sample, load observation, cache check, and diagnostic is retained. The user goal remains unachieved.

Next step: Archive the candidate before restoring its two source preimages and the 6810 CLI. Remove the candidate regression test only after verifying its identical archived copy. Verify all 339 predecessor source identities. Then investigate consolidating the five nested native scratch buffers into two owners, preserving deoptimization, exceptions, recursion, and pin lifetimes.

Candidate: `2ca32ec4fcd0b83be9517edddaa759c058078f6f00fa843005a162b413c3bbbc`, 44,097,264 bytes.
Direct reference: time formatter 6810,
`681043b9f62cbd4fdb3f73756ecdf8ac877c1923b6a509f11ff1128dc973443c`.
Retained acceptance reference: thin value storage bf2,
`bf2db3c58aae37ad8dee45c0859585a32af2cd44c97a2dcaed7a61f9f0df40e4`.

The preceding short-argument trial was deferred. Both source preimages and
the 6810 CLI were restored, and all 339 predecessor source identities matched
before this experiment. The new candidate keeps the preceding default-binding
and time-formatter experiments but excludes the short-argument implementation.

A native CallCtx retains at most one unobserved shell. The cache is empty
throughout each callback, with no cache or mutable shell borrow across Python.
The exact call-site lasti is refreshed before pushing the shell. After the
callback, exclusive ownership is established with Rc::get_mut before the
materialization flag is read. Strong, weak, and frame observers all exclude
reuse. Each of four context constructors starts empty; framed entries pass
through. No new raw pointer, allocator, or ownership representation is added.
Callback arguments and results are not cached. The cache lasts only for its
activation, whose context already owns the shell metadata.

Source review tightened the observation-check order before timing. The first
draft had read the flag before checking exclusive ownership. Its executable
identity, source/method snapshot, and passing 353 VM / 156 C API / lint / no-JIT
results remain under pre-owner-check-v1. This was a source-review correction,
not an observed Python failure or timed regression. Validation was rerun for
the corrected predicate; all timed-candidate checks use that corrected source.

Validation passes 353 VM tests, 156 C API tests, strict format/lint, no-JIT
checking, 99 targeted Python checks, and all 275 outside-sandbox compatibility
checks. Test-only native counters require actual allocation, at least 100
reuses, and at least 10 observer exclusions. The separate Rust test exercises
strong, weak, and materialized observers. No counters are present in release.
The portable oracle covers nested calls, captured code identity, exceptions
without replay, global mutation/deoptimization, and weak-reference lifetimes.
It passes on CPython and predecessor JIT/interpreter/GIL0. GIL0 disables JIT
and provides correctness evidence only. Optional extension checks may test
availability internally.

All 340 source and 151 declared method/input identities remain unchanged.
Static helper and native-entry prologues are preserved under codegen. Reduced
allocation alone is not evidence of lower stack use or peak RSS. A cache per
nested activation is a possible memory cost, covered by the recursive control.
Observed build durations are not controlled build-time comparisons.

The original 26 fixture sources are unchanged. The first 28-input set adds
single-callback and observed-frame controls; it and its baseline traces remain.
The v2 set preserves all 28 and adds recursive callbacks. All 29 independent
CPython checksums pass twice. Untimed traces prove intended generic drivers
compile and exceed 30,000 generic calls, while compiled aliases and the single/
recursive native callees compile and exceed 30,000 native calls. The original
wide-argument and tuple-unpack controls remain interpreted. Inherited formatter
and binding controls retain their observed paths. Coverage is not timing.

A separate frame-identity audit confirms the same known mismatch: five calls
observe one parent-frame identity on CPython and WeavePy interpreter/GIL0,
but distinct identities on both predecessor and candidate JIT. The driver
compiles without deoptimizing. Observed shells are excluded from reuse. This
candidate does not fix native frame identity, full locals materialization,
frame.clear/continuation behavior, or final instruction positions.

Four timing stages run sequentially under the exclusive lease and unchanged
load gate. Every dependent launch follows inspected completion. No owned
build, test, profile, install, runtime edit, or active-harness edit overlaps a
stage. Keep every launched sample, all frozen-cache assertions, failures,
losses, load observations, and VM/swap snapshots. Ratios below 1 favor the
candidate. Means aggregate per-fixture medians of paired ratios. Keep the
historical 21-row, 23-workload, and 24-process cohorts separate. Retained-
baseline results are measured directly, not multiplied across unrelated runs.

## Direct comparison with 6810

| Metric | Candidate / 6810 | Candidate / CPython | Wins over CPython |
| --- | ---: | ---: | ---: |
| JIT workload time (23) | 1.008074180 | 3.397743873 | 6 |
| Interpreter workload time (23) | 1.001362597 | 9.304453681 | 1 |
| JIT process wall time (24) | 1.007975555 | 3.226337880 | 4 |
| JIT CPU time (24) | 1.006083571 | 3.293809613 | 4 |
| JIT peak RSS (24) | 1.001829073 | 2.012242719 | 0 |
| Interpreter process wall time (24) | 0.998370389 | 5.700865646 | 1 |
| Interpreter CPU time (24) | 0.998714230 | 5.895531669 | 1 |
| Interpreter peak RSS (24) | 0.999421618 | 1.835206049 | 0 |

| Focused control | JIT time / 6810 | JIT time / CPython | RSS / 6810 | Time wins (7 pairs) |
| --- | ---: | ---: | ---: | ---: |
| cold/generic_pos_0 | 1.013014 | 12.021318 | 0.999441 | 2 |
| cold/generic_pos_1 | 1.007652 | 10.891791 | 1.002807 | 1 |
| cold/generic_pos_2 | 0.995672 | 10.569325 | 1.000561 | 4 |
| cold/generic_pos_3 | 1.012097 | 9.194809 | 1.003924 | 2 |
| cold/generic_pos_4 | 1.015477 | 8.899358 | 1.001681 | 3 |
| cold/generic_pos_5 | 0.991149 | 8.241163 | 1.001120 | 4 |
| cold/generic_pos_8 | 1.005096 | 7.537240 | 1.003375 | 3 |
| cold/generic_pos_16 | 1.009008 | 6.307473 | 1.001119 | 0 |
| cold/generic_pos_32 | 1.005033 | 8.073131 | 1.003358 | 1 |
| cold/generic_kw_0 | 1.000086 | 7.537272 | 1.003363 | 3 |
| cold/generic_kw_1 | 1.011912 | 7.253021 | 1.000000 | 1 |
| cold/generic_kw_4 | 1.018117 | 6.616778 | 1.001681 | 1 |
| cold/generic_kw_8 | 1.022254 | 5.906403 | 1.003924 | 0 |
| cold/generic_method_1 | 1.005805 | 15.343170 | 1.002250 | 1 |
| cold/generic_method_4 | 1.008007 | 14.797978 | 1.000559 | 1 |
| cold/compiled_alias_1 | 0.995579 | 3.635349 | 1.000558 | 4 |
| cold/compiled_alias_4 | 0.977705 | 2.173675 | 1.000559 | 5 |
| cold/compiled_alias_8 | 0.999610 | 1.637940 | 1.004479 | 4 |
| cold/generic_object_1 | 0.999408 | 12.570435 | 1.001683 | 4 |
| cold/generic_object_4 | 0.989873 | 9.541868 | 1.003941 | 6 |
| cold/generic_object_4_compiled | 0.981944 | 9.816796 | 1.000554 | 5 |
| cold/time_iso_auto | 0.998522 | 8.332631 | 1.003725 | 5 |
| cold/format_callback_fallback | 0.996587 | 7.973462 | 0.999465 | 4 |
| cold/override_64_all_supplied | 0.993922 | 5.528515 | 1.003906 | 5 |
| cold/kw_compiled_64_omitted | 0.997440 | 17.521437 | 0.995556 | 5 |
| cold/kw_override_16_omitted | 1.009345 | 4.907352 | 1.003926 | 1 |
| cold/native_recursive_callback | 1.013002 | 12.643631 | 1.000000 | 1 |
| cold/native_single_callback | 1.013323 | 9.540484 | 1.002800 | 0 |
| cold/native_observed_callback | 0.986046 | 10.755682 | 1.003352 | 6 |
| warm/generic_pos_0 | 0.958113 | 11.585266 | 0.999448 | 7 |
| warm/generic_pos_1 | 0.960825 | 10.655830 | 1.001103 | 7 |
| warm/generic_pos_2 | 0.974488 | 10.171982 | 0.997802 | 7 |
| warm/generic_pos_3 | 0.962614 | 8.866146 | 0.998900 | 7 |
| warm/generic_pos_4 | 0.957349 | 8.273748 | 0.999448 | 7 |
| warm/generic_pos_5 | 0.978538 | 7.994145 | 1.000000 | 7 |
| warm/generic_pos_8 | 0.957458 | 7.116460 | 0.998899 | 7 |
| warm/generic_pos_16 | 0.965096 | 6.084762 | 0.997805 | 7 |
| warm/generic_pos_32 | 1.009543 | 7.988130 | 1.001097 | 2 |
| warm/generic_kw_0 | 0.982410 | 7.454158 | 1.000000 | 7 |
| warm/generic_kw_1 | 0.992980 | 7.200411 | 1.000000 | 6 |
| warm/generic_kw_4 | 0.983419 | 6.538890 | 1.000551 | 5 |
| warm/generic_kw_8 | 0.999075 | 6.001870 | 1.002201 | 4 |
| warm/generic_method_1 | 0.933215 | 15.317170 | 1.001105 | 7 |
| warm/generic_method_4 | 0.940657 | 14.708484 | 1.000000 | 7 |
| warm/compiled_alias_1 | 1.009367 | 2.527065 | 0.997802 | 3 |
| warm/compiled_alias_4 | 0.966470 | 1.428004 | 1.002198 | 5 |
| warm/compiled_alias_8 | 0.995737 | 1.014841 | 1.001650 | 4 |
| warm/generic_object_1 | 0.953836 | 11.874995 | 0.997259 | 7 |
| warm/generic_object_4 | 1.008225 | 9.434349 | 0.997802 | 2 |
| warm/generic_object_4_compiled | 1.037520 | 10.087026 | 0.999453 | 1 |
| warm/time_iso_auto | 0.995317 | 8.032744 | 1.000525 | 4 |
| warm/format_callback_fallback | 0.993431 | 7.961739 | 1.002637 | 6 |
| warm/override_64_all_supplied | 0.996083 | 5.424413 | 1.001646 | 6 |
| warm/kw_compiled_64_omitted | 0.998872 | 17.121597 | 0.998908 | 4 |
| warm/kw_override_16_omitted | 1.004150 | 4.701463 | 1.003291 | 2 |
| warm/native_recursive_callback | 1.013117 | 11.793959 | 0.998921 | 0 |
| warm/native_single_callback | 1.010596 | 9.222414 | 1.004967 | 2 |
| warm/native_observed_callback | 1.002915 | 11.753248 | 1.001098 | 2 |

## Direct comparison with bf2

| Metric | Candidate / bf2 | Candidate / CPython | Wins over CPython |
| --- | ---: | ---: | ---: |
| JIT workload time (23) | 1.007812191 | 3.389340691 | 6 |
| Interpreter workload time (23) | 1.002301104 | 9.274296525 | 1 |
| JIT process wall time (24) | 1.005083330 | 3.182136596 | 4 |
| JIT CPU time (24) | 1.003716686 | 3.252021513 | 4 |
| JIT peak RSS (24) | 1.001762326 | 2.011336976 | 0 |
| Interpreter process wall time (24) | 1.001460775 | 5.672623341 | 1 |
| Interpreter CPU time (24) | 1.001898607 | 5.878877788 | 1 |
| Interpreter peak RSS (24) | 0.999562878 | 1.833286647 | 0 |

| Focused control | JIT time / bf2 | JIT time / CPython | RSS / bf2 | Time wins (7 pairs) |
| --- | ---: | ---: | ---: | ---: |
| cold/generic_pos_0 | 0.989884 | 11.767268 | 1.002242 | 4 |
| cold/generic_pos_1 | 1.010742 | 11.057001 | 1.000560 | 0 |
| cold/generic_pos_2 | 0.994878 | 10.448122 | 1.001120 | 5 |
| cold/generic_pos_3 | 1.004031 | 9.321360 | 0.997210 | 1 |
| cold/generic_pos_4 | 1.019854 | 9.028219 | 1.002236 | 1 |
| cold/generic_pos_5 | 1.010742 | 8.274393 | 1.000000 | 0 |
| cold/generic_pos_8 | 1.035927 | 7.701451 | 1.001116 | 0 |
| cold/generic_pos_16 | 1.030305 | 6.231056 | 0.998326 | 0 |
| cold/generic_pos_32 | 1.031889 | 8.335642 | 0.998885 | 0 |
| cold/generic_kw_0 | 1.002082 | 7.299782 | 1.001118 | 3 |
| cold/generic_kw_1 | 0.995848 | 7.203179 | 0.997204 | 4 |
| cold/generic_kw_4 | 1.020266 | 6.683111 | 1.000560 | 0 |
| cold/generic_kw_8 | 1.006822 | 5.931830 | 1.000000 | 1 |
| cold/generic_method_1 | 1.021492 | 15.709271 | 0.999440 | 2 |
| cold/generic_method_4 | 1.003534 | 15.028293 | 1.002796 | 1 |
| cold/compiled_alias_1 | 0.988796 | 3.532555 | 1.000000 | 4 |
| cold/compiled_alias_4 | 1.002041 | 2.189965 | 1.003919 | 3 |
| cold/compiled_alias_8 | 1.020134 | 1.658071 | 0.996654 | 1 |
| cold/generic_object_1 | 1.016465 | 12.510711 | 0.997767 | 1 |
| cold/generic_object_4 | 0.981907 | 9.570587 | 1.000000 | 6 |
| cold/generic_object_4_compiled | 1.006671 | 9.931165 | 1.000553 | 3 |
| cold/time_iso_auto | 0.616141 | 8.453583 | 1.001066 | 7 |
| cold/format_callback_fallback | 1.061490 | 8.054580 | 1.002681 | 0 |
| cold/override_64_all_supplied | 1.009481 | 6.016174 | 1.000000 | 2 |
| cold/kw_compiled_64_omitted | 1.016708 | 16.783853 | 1.001115 | 0 |
| cold/kw_override_16_omitted | 0.619083 | 4.948224 | 1.003913 | 7 |
| cold/native_recursive_callback | 1.001960 | 12.711437 | 0.997799 | 3 |
| cold/native_single_callback | 0.996694 | 9.678758 | 1.000556 | 5 |
| cold/native_observed_callback | 1.016203 | 11.005323 | 1.000000 | 0 |
| warm/generic_pos_0 | 0.954246 | 11.657756 | 1.000000 | 7 |
| warm/generic_pos_1 | 0.965679 | 10.402815 | 0.998344 | 6 |
| warm/generic_pos_2 | 0.956041 | 10.076393 | 1.000551 | 7 |
| warm/generic_pos_3 | 0.960281 | 8.960173 | 0.998343 | 7 |
| warm/generic_pos_4 | 0.958017 | 8.648616 | 1.000551 | 7 |
| warm/generic_pos_5 | 0.966489 | 7.927872 | 1.001657 | 7 |
| warm/generic_pos_8 | 0.975086 | 7.279849 | 1.001654 | 6 |
| warm/generic_pos_16 | 0.987442 | 6.088159 | 0.999450 | 6 |
| warm/generic_pos_32 | 1.005227 | 8.089424 | 1.000549 | 2 |
| warm/generic_kw_0 | 0.971699 | 7.330867 | 0.999450 | 6 |
| warm/generic_kw_1 | 0.975181 | 7.133419 | 1.001105 | 7 |
| warm/generic_kw_4 | 0.980331 | 6.497959 | 1.000000 | 7 |
| warm/generic_kw_8 | 0.986867 | 6.016457 | 0.998353 | 7 |
| warm/generic_method_1 | 0.948540 | 15.235877 | 1.002209 | 7 |
| warm/generic_method_4 | 0.915111 | 14.420234 | 1.001104 | 7 |
| warm/compiled_alias_1 | 0.981919 | 2.426204 | 1.000000 | 6 |
| warm/compiled_alias_4 | 0.998703 | 1.432612 | 0.998354 | 4 |
| warm/compiled_alias_8 | 1.001252 | 1.027926 | 1.001647 | 3 |
| warm/generic_object_1 | 0.918013 | 12.150680 | 0.999448 | 7 |
| warm/generic_object_4 | 0.981179 | 9.320521 | 1.000000 | 6 |
| warm/generic_object_4_compiled | 1.021700 | 10.006946 | 1.001095 | 0 |
| warm/time_iso_auto | 0.612233 | 8.041592 | 1.000000 | 7 |
| warm/format_callback_fallback | 1.062260 | 8.089289 | 0.996847 | 0 |
| warm/override_64_all_supplied | 1.009228 | 5.719769 | 1.002731 | 2 |
| warm/kw_compiled_64_omitted | 1.020727 | 17.097637 | 0.996725 | 0 |
| warm/kw_override_16_omitted | 0.612769 | 4.716199 | 1.002200 | 7 |
| warm/native_recursive_callback | 1.012936 | 11.530421 | 0.998375 | 0 |
| warm/native_single_callback | 1.015064 | 9.113422 | 1.000000 | 2 |
| warm/native_observed_callback | 0.995456 | 11.827315 | 1.000000 | 4 |

All raw samples, losses, validation outcomes, load observations, VM/swap
snapshots, methods, inputs, and source snapshots remain. Large text artifacts
use lossless gzip; index.json identifies stored and original bytes. Executables
and frozen caches remain local. Intermediate status notes are historical.

The user goal remains unachieved. No universal, energy, controlled build-time,
portability, or native parallel-scaling conclusion follows.

Restoration completed: all 339 predecessor source hashes and the 6810 CLI match.
The candidate-only regression source was removed after verifying its archived copy.
See development/native-unobserved-shell-restoration.json.
