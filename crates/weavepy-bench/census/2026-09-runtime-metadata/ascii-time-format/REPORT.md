# Native formatting for ordinary time fields

Decision: Retain as an experimental foundation for further refinement; bf2 remains the acceptance reference. The user goal remains unachieved.

Against bc88, every JIT pair improves for the six private precisions and three public time.isoformat controls, cold and warm. Private time falls by about 51-63%; public time by about 37-39%. UTC datetime.isoformat improves by about 5-7%.
All seven pairs lose in custom-format, invalid-precision, and negative-field fallbacks in both groups. The respective JIT median costs rise by about 6%, 2-3%, and 9%. Ordinary str.format also records about 1% regressions. No loss is filtered.
Against bf2, the same formatting gains remain, alongside the roughly 39% keyword-default override gain. Wide positional all-supplied calls lose about 2-3%; compiled keyword-only controls lose about 1-2%. Focused RSS is mixed.
The full bc88 comparison records 23-workload JIT time 0.9982564825, 24-process wall 1.0028570169, CPU 1.0016568181, and peak RSS 0.9993274105. JIT median regressions number 11/14/14/10, respectively.
The direct full bf2 comparison records 23-workload JIT time 1.0012292758, 24-process wall 1.0036960571, CPU 1.0028917412, and peak RSS 0.9993201115. JIT median regressions number 10/16/15/7, respectively.
The final 23-workload JIT mean is 3.3920033234 times CPython, with six wins. The 24-process peak-RSS mean is 2.0086893930 times CPython, with zero wins. These measurements do not establish overall superiority.

Next step: Investigate initialized inline argument storage for short generic calls from native code, with explicit stack-frame, lifetime, observability, large-call, and keyword controls. Keep the formatter fallback and broad regressions visible.

Candidate: `681043b9f62cbd4fdb3f73756ecdf8ac877c1923b6a509f11ff1128dc973443c`, 44,097,264 bytes.
Direct reference: combined default borrowing bc88,
`bc88bf2b02abaaf0ad6a3d7427a17cab11f864a400a8161fc2effe5f396dd695`.
Retained acceptance reference: thin value storage bf2,
`bf2db3c58aae37ad8dee45c0859585a32af2cd44c97a2dcaed7a61f9f0df40e4`.

The candidate adds a native formatter for exact integer fields in ordinary
time ranges and exact strings naming six supported precisions. A safe
15-byte ASCII buffer emits zero-padded fields and truncates milliseconds.
Checked UTF-8 conversion produces the final string. There is no unsafe
operation or temporary heap vector. _pydatetime._format_time calls this
optional helper first and keeps its complete previous Python fallback for
unusual values, subclasses, callbacks, and errors. The experimental default-
binding changes from bc88 remain. No other existing runtime source changed.

The standalone three-test prototype matches 7,320 independently generated
CPython time.isoformat outputs. These are correctness results, not runtime
performance measurements. The new Python semantic oracle passes on CPython,
the predecessor, and the candidate. It covers all precisions, boundaries,
random valid fields, timezones and fold, invalid precision, negative/large/
boolean fields, unused custom values, formatting/truth/floor callbacks,
integer/string subclasses, and native-helper rebinding. Independent CPython
checksums match all 17 focused inputs before and after warmup.

Validation: 351 VM tests, 156 C API tests, strict lint/format, no-JIT check,
96 targeted Python checks, native-helper activation proof, and 275 complete
outside-sandbox compatibility checks passed. GIL-disabled checks disable JIT
and are correctness evidence only. Optional extension tests can check their
availability internally. All 339 source and 92 method/input identities
remained unchanged. The observed 4m27s release build is not a controlled
build-time comparison.

A separate untimed path audit completed before timing. Four calls each to
the unchanged datetime and deque fixtures match CPython. JIT rejection and
deoptimization traces motivate follow-up work; they are not timings.

Four stages ran sequentially under the exclusive lease and unchanged host-
load gate: focused and full against bc88, then focused and full against bf2.
Each dependent launch required inspected completion. No owned build, test,
profile, install, runtime edit, or active-harness edit overlapped a stage.
All samples, load observations, and frozen-cache assertions are retained.

Ratios below 1 favor the candidate. Means aggregate per-fixture medians of
paired ratios. Keep the historical 21-row, 23-workload, and 24-process cohorts
separate. Comparisons with bf2 are measured directly in their own stage,
not inferred by multiplying unrelated historical ratios.

## Direct comparison with bc88

| Metric | Candidate / bc88 | Candidate / CPython | Wins over CPython |
| --- | ---: | ---: | ---: |
| JIT workload time (23) | 0.998256482 | 3.364514900 | 6 |
| Interpreter workload time (23) | 1.001006890 | 9.256002384 | 1 |
| JIT process wall time (24) | 1.002857017 | 3.198522789 | 4 |
| JIT CPU time (24) | 1.001656818 | 3.262726970 | 4 |
| JIT peak RSS (24) | 0.999327411 | 2.007086530 | 0 |
| Interpreter process wall time (24) | 0.999467138 | 5.671746894 | 1 |
| Interpreter CPU time (24) | 0.999871661 | 5.863640903 | 1 |
| Interpreter peak RSS (24) | 0.998900091 | 1.832614018 | 0 |

| Focused control | JIT time / bc88 | JIT time / CPython | RSS / bc88 | Time wins (7 pairs) |
| --- | ---: | ---: | ---: | ---: |
| cold/parts_hours | 0.488457 | 3.456885 | 1.001080 | 7 |
| cold/parts_minutes | 0.442722 | 2.753579 | 1.000000 | 7 |
| cold/parts_seconds | 0.407466 | 2.332085 | 1.001078 | 7 |
| cold/parts_milliseconds | 0.373451 | 1.904774 | 1.002155 | 7 |
| cold/parts_microseconds | 0.386870 | 2.002138 | 1.002695 | 7 |
| cold/parts_auto | 0.380281 | 1.988112 | 1.001073 | 7 |
| cold/time_iso_auto | 0.609105 | 8.326377 | 1.000000 | 7 |
| cold/time_iso_milliseconds | 0.611353 | 8.294811 | 1.002137 | 7 |
| cold/time_iso_microseconds | 0.624985 | 8.138456 | 1.002670 | 7 |
| cold/datetime_iso_utc | 0.934999 | 54.625996 | 1.003140 | 7 |
| cold/format_callback_fallback | 1.062568 | 7.943972 | 1.000000 | 0 |
| cold/invalid_precision_fallback | 1.023807 | 43.016647 | 1.001598 | 0 |
| cold/negative_fields_fallback | 1.094420 | 6.040906 | 1.002155 | 0 |
| cold/ordinary_format_control | 1.011217 | 3.820813 | 0.997743 | 0 |
| cold/override_64_all_supplied | 0.993174 | 5.819709 | 1.003908 | 4 |
| cold/kw_compiled_64_omitted | 0.998637 | 17.246604 | 1.004454 | 4 |
| cold/kw_override_16_omitted | 1.003446 | 4.901700 | 1.003348 | 3 |
| warm/parts_hours | 0.484787 | 3.351304 | 1.003181 | 7 |
| warm/parts_minutes | 0.449321 | 2.794207 | 1.005313 | 7 |
| warm/parts_seconds | 0.413994 | 2.345143 | 1.002661 | 7 |
| warm/parts_milliseconds | 0.378458 | 1.941948 | 1.005308 | 7 |
| warm/parts_microseconds | 0.389729 | 2.019350 | 1.004251 | 7 |
| warm/parts_auto | 0.381177 | 2.003056 | 1.000000 | 7 |
| warm/time_iso_auto | 0.607587 | 8.064757 | 1.001576 | 7 |
| warm/time_iso_milliseconds | 0.607498 | 7.912431 | 1.002627 | 7 |
| warm/time_iso_microseconds | 0.615485 | 7.836907 | 1.003153 | 7 |
| warm/datetime_iso_utc | 0.946858 | 54.932593 | 0.999484 | 6 |
| warm/format_callback_fallback | 1.063322 | 8.049070 | 1.001581 | 0 |
| warm/invalid_precision_fallback | 1.033433 | 42.554248 | 1.001047 | 0 |
| warm/negative_fields_fallback | 1.089438 | 5.956907 | 1.004237 | 0 |
| warm/ordinary_format_control | 1.011175 | 3.892533 | 1.002232 | 1 |
| warm/override_64_all_supplied | 0.999892 | 6.232779 | 0.997272 | 4 |
| warm/kw_compiled_64_omitted | 0.999863 | 16.904342 | 1.000545 | 4 |
| warm/kw_override_16_omitted | 0.999659 | 4.830869 | 1.001651 | 4 |

## Direct comparison with bf2

| Metric | Candidate / bf2 | Candidate / CPython | Wins over CPython |
| --- | ---: | ---: | ---: |
| JIT workload time (23) | 1.001229276 | 3.392003323 | 6 |
| Interpreter workload time (23) | 0.999624669 | 9.283453023 | 1 |
| JIT process wall time (24) | 1.003696057 | 3.200017741 | 4 |
| JIT CPU time (24) | 1.002891741 | 3.268491836 | 4 |
| JIT peak RSS (24) | 0.999320112 | 2.008689393 | 0 |
| Interpreter process wall time (24) | 0.998457904 | 5.684148606 | 1 |
| Interpreter CPU time (24) | 0.999188663 | 5.882640940 | 1 |
| Interpreter peak RSS (24) | 1.000516667 | 1.835751371 | 0 |

| Focused control | JIT time / bf2 | JIT time / CPython | RSS / bf2 | Time wins (7 pairs) |
| --- | ---: | ---: | ---: | ---: |
| cold/parts_hours | 0.483895 | 3.401743 | 0.998926 | 7 |
| cold/parts_minutes | 0.443450 | 2.780779 | 1.002152 | 7 |
| cold/parts_seconds | 0.410212 | 2.304714 | 1.002151 | 7 |
| cold/parts_milliseconds | 0.382488 | 1.984613 | 1.002695 | 7 |
| cold/parts_microseconds | 0.385214 | 1.969833 | 1.002147 | 7 |
| cold/parts_auto | 0.385904 | 1.979609 | 1.001076 | 7 |
| cold/time_iso_auto | 0.613358 | 8.247215 | 1.001603 | 7 |
| cold/time_iso_milliseconds | 0.610720 | 8.122768 | 1.000533 | 7 |
| cold/time_iso_microseconds | 0.621375 | 7.968547 | 0.999466 | 7 |
| cold/datetime_iso_utc | 0.937812 | 54.292175 | 0.997399 | 7 |
| cold/format_callback_fallback | 1.061475 | 7.915291 | 1.001074 | 0 |
| cold/invalid_precision_fallback | 1.031341 | 43.739388 | 1.001066 | 0 |
| cold/negative_fields_fallback | 1.078956 | 5.928280 | 1.004301 | 0 |
| cold/ordinary_format_control | 1.021771 | 3.880347 | 0.998872 | 2 |
| cold/override_64_all_supplied | 1.020978 | 6.190492 | 0.997230 | 0 |
| cold/kw_compiled_64_omitted | 1.010614 | 17.098588 | 1.001671 | 2 |
| cold/kw_override_16_omitted | 0.614066 | 4.936120 | 0.994986 | 7 |
| warm/parts_hours | 0.478046 | 3.382142 | 0.998418 | 7 |
| warm/parts_minutes | 0.443041 | 2.754875 | 0.998414 | 7 |
| warm/parts_seconds | 0.416419 | 2.329529 | 1.000000 | 7 |
| warm/parts_milliseconds | 0.378469 | 1.960021 | 0.999473 | 7 |
| warm/parts_microseconds | 0.387901 | 1.977640 | 0.998416 | 7 |
| warm/parts_auto | 0.382127 | 1.987733 | 0.998943 | 7 |
| warm/time_iso_auto | 0.611773 | 8.104068 | 0.998955 | 7 |
| warm/time_iso_milliseconds | 0.609301 | 8.005246 | 0.998429 | 7 |
| warm/time_iso_microseconds | 0.617407 | 7.788184 | 0.997910 | 7 |
| warm/datetime_iso_utc | 0.939094 | 55.331308 | 0.998458 | 7 |
| warm/format_callback_fallback | 1.066367 | 8.046216 | 0.998419 | 0 |
| warm/invalid_precision_fallback | 1.024334 | 42.981638 | 0.998954 | 0 |
| warm/negative_fields_fallback | 1.095338 | 6.052921 | 0.998944 | 0 |
| warm/ordinary_format_control | 1.006358 | 3.944017 | 0.997779 | 3 |
| warm/override_64_all_supplied | 1.028379 | 6.032799 | 0.998362 | 1 |
| warm/kw_compiled_64_omitted | 1.015773 | 16.928341 | 0.999454 | 0 |
| warm/kw_override_16_omitted | 0.613437 | 4.771202 | 1.002747 | 7 |

All raw samples, losses, validation outcomes, load observations, VM/swap
snapshots, methods, inputs, and source snapshots remain. Large text artifacts
use lossless gzip; index.json identifies stored and original bytes. Executables
and frozen caches remain local. Intermediate status notes are historical.

The user goal remains unachieved. No universal, energy, controlled build-time,
portability, or native parallel-scaling conclusion follows.
