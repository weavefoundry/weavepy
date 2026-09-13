# Borrow overridden positional defaults

Decision: preserve this candidate for refinement. The targeted binding gain
is consistent, but the broader JIT process and memory results do not establish
an overall improvement. Retained bf2 remains the acceptance reference. The
candidate remains active only as the source for the next experiment; do not
present it as a universally faster replacement. The user goal is unachieved.

Candidate: `44c5ae82231f44182d2aac1b216d64fccdf712edce4b812baff573d3ea39befb`,
44,096,928 bytes. Reference: retained bf2,
`bf2db3c58aae37ad8dee45c0859585a32af2cd44c97a2dcaed7a61f9f0df40e4`,
44,097,168 bytes. This is a same-run comparison against bf2 only.

The only runtime change is one block in lib.rs. A cloned function-slot value
keeps the override tuple alive while its elements are borrowed. Only defaults
filling missing positional arguments are cloned. The temporary vector of all
overridden defaults is removed. Suffix selection, None clearing, compiled
defaults, keyword-only defaults, errors, and native eligibility remain.
No unsafe operation or ownership/allocation layout was added or changed.

The new Python ownership oracle passes on CPython and bf2 in JIT, interpreter,
and GIL-disabled modes. It covers partial and oversized defaults, code
replacement, bound receivers, errors, aliases, and weak-reference lifetime
when a callee clears its defaults. Independent length/count checks validate
all 12 focused fixture results under CPython.

Validation passed 347 VM tests, 156 C API tests, strict lint and format,
no-JIT compilation, all 90 targeted Python checks, and all 275 outside-sandbox
compatibility checks. Optional extension tests may check availability internally.
The safe borrowing change adds no new raw ownership behavior requiring a
repeat of earlier Miri experiments. All 337 source identities and 72 method/
input identities remained unchanged through the build and measurements.

The release compiler reported 5m21s; an earlier format check was unusually
slow but succeeded. Neither duration is a controlled build-time comparison.
Both timing stages used the serial lease and unchanged outside-sandbox load
gate, completed sequentially, and retained every sample and load observation.

Ratios below 1 favor the candidate. Means aggregate per-fixture medians of
paired ratios. Keep the 21-, 23-, and 24-row cohorts distinct.

| Metric | Candidate / bf2 | Candidate / CPython | Wins over CPython |
| --- | ---: | ---: | ---: |
| JIT workload time (23) | 1.002749018 | 3.401409499 | 6 |
| Interpreter workload time (23) | 0.998534767 | 9.300434354 | 1 |
| JIT process wall time (24) | 1.009901540 | 3.231113290 | 4 |
| JIT CPU time (24) | 1.008663961 | 3.297406450 | 4 |
| JIT peak RSS (24) | 1.001861598 | 2.014088462 | 0 |
| Interpreter process wall time (24) | 0.996302820 | 5.691460615 | 1 |
| Interpreter CPU time (24) | 0.996141306 | 5.885768194 | 1 |
| Interpreter peak RSS (24) | 0.999999456 | 1.838471098 | 0 |

All seven JIT pairs improve in every actually affected override-default
control, both cold and warm: roughly 2-12% less workload time. The width-one
partial-keyword case supplies every argument and is an unaffected control.
Some all-supplied and compiled-default controls regress about 1-2.5%. Peak
RSS does not improve consistently. These losses remain in the table and data.
The full JIT suite records 15 workload, 19 process-wall/CPU, and 16 peak-RSS
median regressions against bf2. Interpreter aggregates improve slightly.
No confidence or noise argument removes the observed JIT regressions.

| Focused control | Time / bf2 | RSS / bf2 | Time wins (7 pairs) |
| --- | ---: | ---: | ---: |
| cold/override_1_omitted | 0.964607 | 1.002227 | 7 |
| cold/override_1_partial_keyword | 0.997614 | 1.001677 | 4 |
| cold/override_4_omitted | 0.934565 | 0.999443 | 7 |
| cold/override_4_partial_keyword | 0.953251 | 1.000000 | 7 |
| cold/override_16_omitted | 0.929352 | 1.001114 | 7 |
| cold/override_16_partial_keyword | 0.953811 | 0.998890 | 7 |
| cold/override_64_omitted | 0.895996 | 0.999442 | 7 |
| cold/override_64_partial_keyword | 0.911815 | 1.000558 | 7 |
| cold/override_4_all_supplied | 1.001739 | 1.001666 | 3 |
| cold/compiled_4_omitted | 1.001376 | 1.000560 | 3 |
| cold/override_64_all_supplied | 1.019538 | 1.005036 | 1 |
| cold/compiled_64_omitted | 0.996128 | 1.001113 | 4 |
| warm/override_1_omitted | 0.977395 | 1.004405 | 7 |
| warm/override_1_partial_keyword | 1.001709 | 1.002755 | 3 |
| warm/override_4_omitted | 0.948982 | 1.001653 | 7 |
| warm/override_4_partial_keyword | 0.957206 | 1.002741 | 7 |
| warm/override_16_omitted | 0.935924 | 1.000000 | 7 |
| warm/override_16_partial_keyword | 0.932693 | 1.004388 | 7 |
| warm/override_64_omitted | 0.884276 | 1.002195 | 7 |
| warm/override_64_partial_keyword | 0.910940 | 1.001644 | 7 |
| warm/override_4_all_supplied | 1.015496 | 1.001092 | 1 |
| warm/compiled_4_omitted | 1.007178 | 1.003311 | 2 |
| warm/override_64_all_supplied | 1.025246 | 1.002182 | 2 |
| warm/compiled_64_omitted | 1.001661 | 1.000546 | 2 |

All raw samples, losses, validation outcomes, load observations, VM/swap
snapshots, methods, inputs, and source snapshots remain. Larger text artifacts
use lossless gzip; index.json records stored and original hashes. Executables
and frozen caches remain local. Intermediate status notes are historical.

No universal, energy, controlled build-time, portability, or native parallel-
scaling conclusion follows from these measurements.
