# Initialized arguments for short generic native calls

Decision: Defer the initialized short-argument candidate; restore the 6810 predecessor before the next experiment.

The 6810 comparison improves several one-through-four-argument controls by about 2% to 5%, but zero-argument, larger, keyword, and compiled controls are mixed. The 26-control cold/warm JIT means are 0.991884938 and 0.993278071; peak-RSS means are 1.003522477 and 1.001391378.
Against 6810, the unchanged 23-workload JIT mean is 1.001654330. The 24-process wall/CPU/RSS means are 1.009832228, 1.007867097, and 1.002255011. Thirteen workload rows and eighteen RSS rows regress. These results do not establish an overall improvement.
Against retained bf2, the 23-workload JIT mean is 0.999706219. The 24-process wall/CPU/RSS means are 1.005005560, 1.005473430, and 1.002643502. Eleven workload rows and nineteen RSS rows regress. The workload mean is 3.391137429 times CPython, with six wins; peak RSS is 2.012896947 times CPython, with no wins.
The historical 21-row JIT mean against CPython is 2.085352628 in the bf2 comparison. Keep this cohort separate from the 23-workload and 24-process means; differences between independently timed CPython references are not proof of a regression.
The retained-baseline focused run still exposes inherited formatter/default-binding gains and fallback losses. Its cold/warm time means are 0.960404816 and 0.957999934. The sixteen-argument warm control loses all seven pairs at 1.024158; callback formatting and compiled wide keyword defaults also lose every warm pair.
The binary shrinks by 16,512 bytes. Each entry frame shrinks from 448 to 96 bytes, but the generic helper plus live caller uses 720 bytes instead of 448. This stack tradeoff and measured RSS increases do not meet the stated memory objective.
All declared validation passes, and all four timing stages complete sequentially under the exclusive lease. A separate frame-identity audit exposes the same preexisting mismatch in the predecessor and candidate; it remains unresolved. Every raw measurement, failed development attempt, source identity, and diagnostic is retained.

Next step: Restore the two runtime/test source preimages and the 6810 CLI after archiving. Remove the candidate-specific regression source from the active tree only after its byte-identical copy is verified in the archive. Verify all 339 predecessor source hashes. Then test per-activation reuse of exclusively owned, unmaterialized frame shells, preserving the separate observed-frame limitations.

Candidate: `ebb53a9a116b16b9101f43f87c18bb4a20a5f83a9e3fd8d0bc056c87a5a7985c`, 44,080,752 bytes.
Direct reference: time formatter 6810,
`681043b9f62cbd4fdb3f73756ecdf8ac877c1923b6a509f11ff1128dc973443c`.
Retained acceptance reference: thin value storage bf2,
`bf2db3c58aae37ad8dee45c0859585a32af2cd44c97a2dcaed7a61f9f0df40e4`.

The runtime change outlines generic dynamic-call dispatch. Up to four
positional arguments use an initialized four-Object array; larger positional
and all keyword calls retain the vector path. Argument order and ownership,
keyword-tail moves, activation shells, dirty marking, global guards, parked
results, integer-result exits, and errors keep their existing behavior.
There is no new uninitialized Object storage or ownership representation.
Existing compiled-callee entry precedes the outlined generic helper. Earlier
default-binding and formatter changes remain in this candidate.

Static machine-code inspection records both call-entry frames shrinking from
448 to 96 bytes. The new generic helper reserves 624 bytes, including its
96-byte register save. The entry frame stays live across that call, so the
combined generic path reserves 720 bytes instead of 448. This is a stack
tradeoff, not a lower generic-stack or RSS claim. The executable shrinks by
16,512 bytes. The observed 4m05s release build is not a controlled build-time
comparison. Original rejected objdump invocations and corrected output remain.

The Python semantic oracle passes on CPython, the predecessor, and the new
candidate in their stated modes. It covers arities, aliases, keyword order,
nested calls, weak-reference lifetimes, mixed returns, exceptions, global
mutation inside a callback, and frame visibility. The VM regression asserts
at least 100 executions in each of seven argument paths with test-only
counters. No such counter is present in release code. Initial test-only Cell
qualification and format-check failures are preserved with their correction.

Validation passed 352 VM tests, 156 C API tests, strict format/lint, a no-JIT
check, 99 targeted Python checks, and all 275 outside-sandbox compatibility
checks. Optional extension tests may check availability internally. GIL-
disabled checks disable JIT and are correctness evidence only. All 340
source identities and 132 method/input identities stayed unchanged.

The original 25 fixture sources and traces remain. The v2 set preserves
every original source and adds a four-object case using explicit assignments.
All 26 independent CPython checksum oracles pass twice. Untimed predecessor
and candidate traces prove the intended generic drivers compile and make
more than 30,000 generic calls; compiled-alias controls compile and make
more than 30,000 native-to-native calls. Wide 32-argument and original four-
object drivers remain interpreted, as do several inherited controls. These
coverage captures are separate from performance measurements.

A separate untimed frame-identity diagnostic completed before timing. Five
callbacks from one driver capture the same parent frame in CPython and in
WeavePy interpreter mode. Both the predecessor and candidate JIT builds
return distinct frame objects, although their code names are correct. This
preexisting mismatch remains and motivates the next allocation/observability
investigation. Passing the declared suite does not imply universal semantic
equivalence. All diagnostic outputs and traces are preserved.

The four timing stages ran sequentially under the exclusive lease and
unchanged host-load gate. Every dependent launch followed inspected
completion. No owned build, test, profile, install, runtime edit, or active-
harness edit overlapped a stage. All samples and frozen-cache assertions
remain, including losses and all load observations.

Ratios below 1 favor the candidate. Means aggregate per-fixture medians of
paired ratios. Keep the historical 21-row, 23-workload, and 24-process cohorts
separate. The bf2 comparisons are measured directly, not inferred by
multiplying ratios from unrelated runs.

## Direct comparison with 6810

| Metric | Candidate / 6810 | Candidate / CPython | Wins over CPython |
| --- | ---: | ---: | ---: |
| JIT workload time (23) | 1.001654330 | 3.354750913 | 6 |
| Interpreter workload time (23) | 1.000425263 | 9.264546221 | 1 |
| JIT process wall time (24) | 1.009832228 | 3.205823070 | 4 |
| JIT CPU time (24) | 1.007867097 | 3.270935016 | 4 |
| JIT peak RSS (24) | 1.002255011 | 2.012550758 | 0 |
| Interpreter process wall time (24) | 0.998456881 | 5.665165434 | 1 |
| Interpreter CPU time (24) | 0.998702561 | 5.863827100 | 1 |
| Interpreter peak RSS (24) | 1.002622591 | 1.840929427 | 0 |

| Focused control | JIT time / 6810 | JIT time / CPython | RSS / 6810 | Time wins (7 pairs) |
| --- | ---: | ---: | ---: | ---: |
| cold/generic_pos_0 | 1.004662 | 11.890978 | 1.002807 | 3 |
| cold/generic_pos_1 | 0.973536 | 10.586973 | 1.002804 | 6 |
| cold/generic_pos_2 | 0.949466 | 10.240060 | 1.002240 | 5 |
| cold/generic_pos_3 | 0.979923 | 9.088788 | 1.003906 | 7 |
| cold/generic_pos_4 | 0.984059 | 8.607888 | 1.006166 | 6 |
| cold/generic_pos_5 | 1.003921 | 8.278945 | 1.006190 | 3 |
| cold/generic_pos_8 | 0.977402 | 7.445351 | 1.001121 | 7 |
| cold/generic_pos_16 | 0.996252 | 6.211303 | 1.005045 | 4 |
| cold/generic_pos_32 | 1.007349 | 8.346865 | 1.005045 | 2 |
| cold/generic_kw_0 | 1.005213 | 7.442137 | 1.004492 | 3 |
| cold/generic_kw_1 | 1.013020 | 7.370673 | 1.003917 | 2 |
| cold/generic_kw_4 | 0.989926 | 6.597810 | 1.005042 | 6 |
| cold/generic_kw_8 | 0.989060 | 5.840388 | 1.001676 | 5 |
| cold/generic_method_1 | 0.989251 | 15.364991 | 1.003917 | 5 |
| cold/generic_method_4 | 0.974210 | 14.655837 | 1.004494 | 6 |
| cold/compiled_alias_1 | 0.994269 | 3.647107 | 1.000000 | 5 |
| cold/compiled_alias_4 | 0.996658 | 2.168389 | 1.001676 | 4 |
| cold/compiled_alias_8 | 1.005572 | 1.612855 | 1.006173 | 3 |
| cold/generic_object_1 | 0.962154 | 12.111877 | 1.001677 | 7 |
| cold/generic_object_4 | 1.012741 | 9.523322 | 1.002806 | 2 |
| cold/generic_object_4_compiled | 0.984568 | 9.752197 | 1.005546 | 7 |
| cold/time_iso_auto | 0.988635 | 8.406426 | 1.001598 | 4 |
| cold/format_callback_fallback | 1.003092 | 7.996279 | 1.002687 | 3 |
| cold/override_64_all_supplied | 0.994638 | 5.966403 | 1.006142 | 4 |
| cold/kw_compiled_64_omitted | 1.012527 | 17.528987 | 1.000000 | 2 |
| cold/kw_override_16_omitted | 1.000105 | 4.983879 | 1.004462 | 3 |
| warm/generic_pos_0 | 1.010206 | 12.151280 | 0.998353 | 1 |
| warm/generic_pos_1 | 0.976425 | 10.883649 | 1.002203 | 6 |
| warm/generic_pos_2 | 0.968119 | 10.367237 | 1.001652 | 7 |
| warm/generic_pos_3 | 0.978547 | 8.845596 | 1.000552 | 7 |
| warm/generic_pos_4 | 0.962257 | 8.999419 | 1.001103 | 6 |
| warm/generic_pos_5 | 1.009343 | 8.449936 | 1.001104 | 2 |
| warm/generic_pos_8 | 0.991285 | 7.415973 | 1.002770 | 7 |
| warm/generic_pos_16 | 1.009264 | 6.313758 | 1.003300 | 1 |
| warm/generic_pos_32 | 1.001972 | 7.977542 | 1.003855 | 3 |
| warm/generic_kw_0 | 1.002386 | 7.508662 | 0.998351 | 3 |
| warm/generic_kw_1 | 1.007965 | 7.316757 | 1.000000 | 2 |
| warm/generic_kw_4 | 1.002419 | 6.551923 | 1.001654 | 3 |
| warm/generic_kw_8 | 1.005151 | 5.907600 | 1.002199 | 2 |
| warm/generic_method_1 | 0.984188 | 15.677142 | 1.002209 | 7 |
| warm/generic_method_4 | 0.977316 | 14.800438 | 1.001652 | 7 |
| warm/compiled_alias_1 | 0.994801 | 2.531298 | 1.001652 | 5 |
| warm/compiled_alias_4 | 0.986565 | 1.372503 | 1.002206 | 4 |
| warm/compiled_alias_8 | 0.972616 | 1.002140 | 1.002199 | 6 |
| warm/generic_object_1 | 0.983045 | 12.344843 | 1.003853 | 6 |
| warm/generic_object_4 | 1.001430 | 9.139447 | 0.997805 | 2 |
| warm/generic_object_4_compiled | 0.989674 | 9.604919 | 1.000000 | 6 |
| warm/time_iso_auto | 1.007629 | 8.054965 | 1.001572 | 3 |
| warm/format_callback_fallback | 0.995259 | 7.992002 | 1.001581 | 5 |
| warm/override_64_all_supplied | 0.999463 | 5.779908 | 1.001095 | 4 |
| warm/kw_compiled_64_omitted | 1.001688 | 17.108832 | 1.001638 | 2 |
| warm/kw_override_16_omitted | 1.008774 | 4.610842 | 1.001647 | 2 |

## Direct comparison with bf2

| Metric | Candidate / bf2 | Candidate / CPython | Wins over CPython |
| --- | ---: | ---: | ---: |
| JIT workload time (23) | 0.999706219 | 3.391137429 | 6 |
| Interpreter workload time (23) | 1.001711394 | 9.290391369 | 1 |
| JIT process wall time (24) | 1.005005560 | 3.200556387 | 4 |
| JIT CPU time (24) | 1.005473430 | 3.267537133 | 4 |
| JIT peak RSS (24) | 1.002643502 | 2.012896947 | 0 |
| Interpreter process wall time (24) | 1.000627448 | 5.676686285 | 1 |
| Interpreter CPU time (24) | 1.001630183 | 5.878599445 | 1 |
| Interpreter peak RSS (24) | 1.003808079 | 1.841455080 | 0 |

| Focused control | JIT time / bf2 | JIT time / CPython | RSS / bf2 | Time wins (7 pairs) |
| --- | ---: | ---: | ---: | ---: |
| cold/generic_pos_0 | 0.998391 | 11.865848 | 0.999441 | 6 |
| cold/generic_pos_1 | 0.983947 | 10.528814 | 1.003367 | 7 |
| cold/generic_pos_2 | 0.988246 | 10.177904 | 1.001121 | 6 |
| cold/generic_pos_3 | 0.978036 | 9.045314 | 1.003369 | 6 |
| cold/generic_pos_4 | 0.969421 | 8.732111 | 1.002238 | 7 |
| cold/generic_pos_5 | 0.995433 | 8.198038 | 1.001681 | 5 |
| cold/generic_pos_8 | 1.000459 | 7.361131 | 0.998326 | 3 |
| cold/generic_pos_16 | 1.022821 | 6.195497 | 1.001116 | 1 |
| cold/generic_pos_32 | 1.019383 | 8.332954 | 1.002238 | 1 |
| cold/generic_kw_0 | 0.995690 | 7.604891 | 1.002246 | 4 |
| cold/generic_kw_1 | 0.979728 | 7.431001 | 1.003926 | 4 |
| cold/generic_kw_4 | 0.988966 | 6.396019 | 1.001118 | 6 |
| cold/generic_kw_8 | 0.990216 | 5.716098 | 1.000558 | 5 |
| cold/generic_method_1 | 0.985231 | 15.447745 | 1.002811 | 5 |
| cold/generic_method_4 | 0.984207 | 14.548201 | 1.004477 | 6 |
| cold/compiled_alias_1 | 0.977831 | 3.612365 | 0.999440 | 5 |
| cold/compiled_alias_4 | 1.014344 | 2.273322 | 1.001674 | 2 |
| cold/compiled_alias_8 | 1.013065 | 1.645006 | 1.001117 | 2 |
| cold/generic_object_1 | 0.989103 | 12.101165 | 1.005042 | 5 |
| cold/generic_object_4 | 0.984537 | 9.620958 | 1.003941 | 4 |
| cold/generic_object_4_compiled | 0.984606 | 9.629469 | 1.001106 | 6 |
| cold/time_iso_auto | 0.617292 | 8.426169 | 1.002128 | 7 |
| cold/format_callback_fallback | 1.067435 | 7.900906 | 1.003212 | 0 |
| cold/override_64_all_supplied | 0.997780 | 6.325412 | 1.002778 | 4 |
| cold/kw_compiled_64_omitted | 1.014966 | 17.438749 | 1.002789 | 0 |
| cold/kw_override_16_omitted | 0.614545 | 4.953645 | 1.003917 | 7 |
| warm/generic_pos_0 | 0.995683 | 12.078128 | 1.002758 | 4 |
| warm/generic_pos_1 | 0.977643 | 10.837924 | 1.000000 | 6 |
| warm/generic_pos_2 | 0.989005 | 10.352253 | 1.003306 | 4 |
| warm/generic_pos_3 | 0.962322 | 9.068418 | 1.000553 | 7 |
| warm/generic_pos_4 | 0.976184 | 8.868562 | 1.001101 | 7 |
| warm/generic_pos_5 | 0.994231 | 8.197645 | 1.002206 | 5 |
| warm/generic_pos_8 | 1.001124 | 7.336708 | 1.002759 | 3 |
| warm/generic_pos_16 | 1.024158 | 6.238455 | 1.002201 | 0 |
| warm/generic_pos_32 | 0.998458 | 7.899812 | 0.997805 | 6 |
| warm/generic_kw_0 | 0.994615 | 7.491164 | 0.999448 | 4 |
| warm/generic_kw_1 | 0.996453 | 7.425362 | 1.001102 | 4 |
| warm/generic_kw_4 | 0.977572 | 6.650240 | 1.003863 | 6 |
| warm/generic_kw_8 | 0.994600 | 6.002866 | 1.001102 | 5 |
| warm/generic_method_1 | 0.991630 | 15.398721 | 1.001099 | 6 |
| warm/generic_method_4 | 0.980147 | 14.809552 | 1.002205 | 7 |
| warm/compiled_alias_1 | 1.018746 | 2.440596 | 1.003311 | 3 |
| warm/compiled_alias_4 | 0.998320 | 1.368697 | 1.002205 | 4 |
| warm/compiled_alias_8 | 1.003335 | 1.035108 | 1.001653 | 3 |
| warm/generic_object_1 | 0.942153 | 12.295558 | 1.002755 | 7 |
| warm/generic_object_4 | 0.990157 | 9.444850 | 1.002773 | 4 |
| warm/generic_object_4_compiled | 0.997474 | 9.654929 | 0.999456 | 4 |
| warm/time_iso_auto | 0.610050 | 7.983737 | 0.997384 | 7 |
| warm/format_callback_fallback | 1.053737 | 8.040506 | 0.998948 | 0 |
| warm/override_64_all_supplied | 1.015699 | 5.607488 | 1.000000 | 0 |
| warm/kw_compiled_64_omitted | 1.013028 | 16.975767 | 1.000000 | 0 |
| warm/kw_override_16_omitted | 0.605250 | 4.728114 | 1.000549 | 7 |

All raw samples, losses, validation outcomes, load observations, VM/swap
snapshots, methods, inputs, and source snapshots remain. Large text artifacts
use lossless gzip; index.json identifies stored and original bytes. Executables
and frozen caches remain local. Intermediate status notes are historical.

The user goal remains unachieved. No universal, energy, controlled build-time,
portability, or native parallel-scaling conclusion follows.

Restoration completed after export: both source preimages and the 6810 CLI are
restored, the candidate-only regression source is retained in this archive, and
all 339 predecessor source hashes match. See the restoration proof in development.
