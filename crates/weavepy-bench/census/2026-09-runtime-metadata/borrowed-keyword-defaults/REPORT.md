# Borrow positional and keyword-only defaults

Decision: retain as an experimental foundation for further refinement. The
targeted gains are strong and correctness checks pass, but broader JIT time
and RSS remain slightly worse. Retained bf2 remains the acceptance reference.
Do not describe this as an overall or universal improvement over bf2 or
CPython. The user goal remains unachieved.

Candidate: `bc88bf2b02abaaf0ad6a3d7427a17cab11f864a400a8161fc2effe5f396dd695`,
44,080,336 bytes. Measured reference: retained bf2,
`bf2db3c58aae37ad8dee45c0859585a32af2cd44c97a2dcaed7a61f9f0df40e4`,
44,097,168 bytes. Source predecessor is positional-only 44c5; no new direct
performance comparison against 44c5 is claimed.

The combined runtime change affects two adjacent default-binding blocks in
lib.rs. Positional defaults borrow the pinned override tuple. Keyword-only
defaults borrow the override dictionary and preserve plain-string iteration
and order. Both clone only values filling missing arguments. Keyword default
processing is skipped when all keyword-only arguments are already supplied.
No string-key hash probe can invoke exotic equality under the borrow. All
borrows end before user code and error rendering. Compiled defaults, None
clearing, suffix rules, and native eligibility retain their existing behavior.
No unsafe operation or ownership/allocation layout was added or changed.

The new keyword oracle passes on CPython and bf2 in JIT, interpreter, and
GIL-disabled modes. It checks sparse/all-supplied calls, unrelated keys,
aliases, None/deletion, code replacement, errors, bound receivers, and
clearing the defaults dictionary inside the callee. Required values stay
alive; unused values do not acquire a hidden lifetime. The positional oracle
also remains. All 24 fixture checksums match independently calculated CPython
results; the earlier positional fixture sources are unchanged.

Validation passed 347 VM tests, 156 C API tests, strict lint/format, a no-JIT
check, all 93 targeted Python checks, and all 275 outside-sandbox compatibility
checks. Optional extension tests may check availability internally. This safe
borrowing change does not add raw ownership operations requiring new Miri
trials. All 338 source identities and 120 method/input identities remained
unchanged through building and measurement. The observed 4m03s release build
is not a controlled build-time comparison.

Both stages used the exclusive serial lease and unchanged outside-sandbox
load gate. The full stage launched only after focused completion was inspected.
All samples, load observations, and frozen-cache assertions are retained.

Ratios below 1 favor the candidate. Means aggregate per-fixture medians of
paired ratios. Keep the 21-, 23-, and 24-row cohorts distinct.

| Metric | Candidate / bf2 | Candidate / CPython | Wins over CPython |
| --- | ---: | ---: | ---: |
| JIT workload time (23) | 1.002080781 | 3.381829043 | 6 |
| Interpreter workload time (23) | 0.999723004 | 9.295092094 | 1 |
| JIT process wall time (24) | 1.005706766 | 3.210446535 | 4 |
| JIT CPU time (24) | 1.004100354 | 3.282114520 | 4 |
| JIT peak RSS (24) | 1.000489385 | 2.010865863 | 0 |
| Interpreter process wall time (24) | 0.996202990 | 5.670413443 | 1 |
| Interpreter CPU time (24) | 0.996760353 | 5.867239758 | 1 |
| Interpreter peak RSS (24) | 0.999370472 | 1.837634206 | 0 |

Every JIT pair improves in each keyword-only override control, cold and
warm, with roughly 11-39% less time. Positional override gains remain. Wide
positional all-supplied and compiled keyword-only controls retain roughly
1-2% regressions. Peak RSS is mixed, not consistently improved. Full JIT
workload/wall/CPU/RSS record 12/16/17/12 median regressions respectively.
Interpreter aggregates improve slightly. No confidence or noise argument
removes the observed losses.

| Focused control | Time / bf2 | RSS / bf2 | Time wins (7 pairs) |
| --- | ---: | ---: | ---: |
| cold/override_1_omitted | 0.959807 | 1.001115 | 7 |
| cold/override_1_partial_keyword | 0.984328 | 1.001114 | 7 |
| cold/override_4_omitted | 0.948290 | 1.000000 | 7 |
| cold/override_4_partial_keyword | 0.949503 | 1.000000 | 7 |
| cold/override_16_omitted | 0.915789 | 1.003915 | 7 |
| cold/override_16_partial_keyword | 0.930415 | 1.001113 | 7 |
| cold/override_64_omitted | 0.903436 | 1.000000 | 7 |
| cold/override_64_partial_keyword | 0.914964 | 0.997779 | 7 |
| cold/override_4_all_supplied | 0.992452 | 0.998890 | 4 |
| cold/compiled_4_omitted | 0.983492 | 0.997213 | 6 |
| cold/override_64_all_supplied | 1.018965 | 0.997230 | 1 |
| cold/compiled_64_omitted | 0.996022 | 1.001675 | 4 |
| cold/kw_override_1_omitted | 0.878857 | 0.998329 | 7 |
| cold/kw_override_1_partial_keyword | 0.866399 | 1.001669 | 7 |
| cold/kw_override_4_omitted | 0.767545 | 1.002231 | 7 |
| cold/kw_override_4_partial_keyword | 0.827693 | 0.998883 | 7 |
| cold/kw_override_16_omitted | 0.610024 | 1.002793 | 7 |
| cold/kw_override_16_partial_keyword | 0.658701 | 1.001675 | 7 |
| cold/kw_override_64_omitted | 0.776312 | 1.001119 | 7 |
| cold/kw_override_64_partial_keyword | 0.788665 | 1.003900 | 7 |
| cold/kw_override_4_all_supplied | 0.776915 | 1.002221 | 7 |
| cold/kw_compiled_4_omitted | 0.996921 | 0.998322 | 4 |
| cold/kw_override_64_all_supplied | 0.664233 | 1.001682 | 7 |
| cold/kw_compiled_64_omitted | 1.017564 | 1.002782 | 0 |
| warm/override_1_omitted | 0.957682 | 1.002206 | 7 |
| warm/override_1_partial_keyword | 0.980315 | 1.000000 | 6 |
| warm/override_4_omitted | 0.953057 | 1.001652 | 7 |
| warm/override_4_partial_keyword | 0.954243 | 1.003295 | 7 |
| warm/override_16_omitted | 0.924949 | 1.001100 | 7 |
| warm/override_16_partial_keyword | 0.933710 | 1.000550 | 7 |
| warm/override_64_omitted | 0.892040 | 1.000550 | 7 |
| warm/override_64_partial_keyword | 0.911161 | 1.000000 | 7 |
| warm/override_4_all_supplied | 1.004637 | 1.001095 | 3 |
| warm/compiled_4_omitted | 1.007181 | 0.998900 | 3 |
| warm/override_64_all_supplied | 1.021152 | 0.997814 | 0 |
| warm/compiled_64_omitted | 0.997326 | 0.996725 | 4 |
| warm/kw_override_1_omitted | 0.885505 | 0.999452 | 7 |
| warm/kw_override_1_partial_keyword | 0.852665 | 0.998900 | 7 |
| warm/kw_override_4_omitted | 0.768515 | 0.999451 | 7 |
| warm/kw_override_4_partial_keyword | 0.814215 | 0.998902 | 7 |
| warm/kw_override_16_omitted | 0.608210 | 1.001651 | 7 |
| warm/kw_override_16_partial_keyword | 0.648760 | 1.000550 | 7 |
| warm/kw_override_64_omitted | 0.785349 | 1.001101 | 7 |
| warm/kw_override_64_partial_keyword | 0.790552 | 1.003848 | 7 |
| warm/kw_override_4_all_supplied | 0.772561 | 0.998907 | 7 |
| warm/kw_compiled_4_omitted | 0.997173 | 1.003865 | 5 |
| warm/kw_override_64_all_supplied | 0.660324 | 1.000000 | 7 |
| warm/kw_compiled_64_omitted | 1.010216 | 0.998907 | 1 |

All raw samples, losses, validation outcomes, load observations, VM/swap
snapshots, methods, inputs, and source snapshots remain. Large text artifacts
use lossless gzip; index.json identifies stored and original bytes. Executables
and frozen caches remain local. Intermediate status notes are historical.

A separate time-formatting prototype was prepared during compatibility checks
and finished before any measurement gate launched. It did not change this
candidate and is outside this trial. Its correctness results are not runtime
performance measurements.

No universal, energy, controlled build-time, portability, or native parallel-
scaling conclusion follows from these measurements.
