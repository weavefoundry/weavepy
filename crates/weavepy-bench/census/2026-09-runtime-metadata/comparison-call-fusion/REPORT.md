# Fuse comparison call results

Decision: deferred. The overall performance goal remains unachieved.

Candidate c27713f0 reuses the existing exact-integer CallDyn result for an
adjacent comparison or one separated only by a non-guarding scalar constant.
Other shapes retain their comparison-PC guard. Noninteger values and invalidated
globals park the completed return and resume after its producing call. Callback
effects, comparison results, and errors are preserved. No runtime helper, pin
lifetime, cache, execution policy, object field, or unsafe code changes.
The binary is 44,289,664 bytes, 128 bytes larger than immediate e709 and 144
bytes larger than the preceding assertion build ed99. Earlier optimizations
and their regressions are included. No build-time advantage is established.

All 60 JIT tests, 342 VM tests, formatting, Clippy, and no-default-feature
checks pass. The original 75 targeted release checks and all six nonempty
extra comparison/integer/call/unittest suites pass. All 275 standard
compatibility checks pass. Original and corrected baseline evidence remains.

The first native-coverage assertion failed: cmp_fusion_local remained in the
interpreter after an unseeded MixedArithTypes rejection. Its semantic pass was
not native coverage. The corrected integer-only fixture adds n = n + 0 before
comparison to establish the local lane, then passes CPython and unchanged e709
in three modes. It also passes all three c277 modes, and all 23 required
functions actually compile and rebuild. Only the Python test changed after
building; Rust sources and binary identity stayed unchanged. Original and
post-build corrected snapshots, methods, and failure logs remain separate.
Timing used only the v2 launcher requiring both corrected checks.

The focused comparison retains 15 warm and four cold controls, work 5000,
seven pairs, and both modes of candidate, immediate e709, retained ed99, and
CPython. All inputs and drivers match the unfused experiment. All values and
frozen caches agree. GIL-disabled execution is a correctness control only.
Lower ratios are better. The ed99 reference measures the combined comparison
change; the e709 predecessor isolates this fusion refinement.

| Case | Mode | Time / unfused e709 | Time / ed99 | RSS / ed99 | Time / CPython |
| --- | --- | ---: | ---: | ---: | ---: |
| warm/left_int | jit | 0.9650 | 0.7685 | 0.9968 | 7.0096 |
| warm/left_int | interp | 1.0041 | 0.9909 | 0.9953 | 10.3788 |
| warm/right_int | jit | 0.9642 | 0.7613 | 1.0000 | 7.2262 |
| warm/right_int | interp | 0.9869 | 0.9840 | 0.9935 | 10.6053 |
| warm/branch_int | jit | 0.8971 | 0.3338 | 0.9973 | 3.9934 |
| warm/branch_int | interp | 0.9963 | 0.9979 | 0.9924 | 11.3945 |
| warm/bool_miss | jit | 1.0049 | 0.9924 | 0.9989 | 11.6835 |
| warm/bool_miss | interp | 1.0183 | 0.9916 | 0.9941 | 11.1310 |
| warm/float_miss | jit | 1.0107 | 1.0041 | 1.0016 | 12.1651 |
| warm/float_miss | interp | 0.9983 | 0.9994 | 0.9947 | 10.7240 |
| warm/bigint_miss | jit | 0.9968 | 0.9866 | 0.9995 | 12.2118 |
| warm/bigint_miss | interp | 0.9844 | 0.9899 | 0.9918 | 12.2499 |
| warm/rich_every_0 | jit | 0.9884 | 1.1091 | 0.9984 | 12.5447 |
| warm/rich_every_0 | interp | 0.9829 | 0.9907 | 0.9959 | 11.2615 |
| warm/rich_every_100 | jit | 0.9868 | 1.0066 | 1.0005 | 10.7402 |
| warm/rich_every_100 | interp | 0.9965 | 0.9892 | 0.9941 | 10.5961 |
| warm/rich_every_2 | jit | 1.0045 | 1.0056 | 1.0000 | 10.6672 |
| warm/rich_every_2 | interp | 1.0088 | 1.0002 | 0.9941 | 10.1001 |
| warm/rich_every_1 | jit | 0.9872 | 0.9914 | 1.0022 | 12.7858 |
| warm/rich_every_1 | interp | 1.0013 | 0.9974 | 0.9941 | 11.7945 |
| warm/miss_burst_recovery | jit | 0.9937 | 1.0183 | 1.0005 | 9.6667 |
| warm/miss_burst_recovery | interp | 0.9866 | 0.9856 | 0.9976 | 10.5377 |
| warm/date_toordinal | jit | 0.9970 | 0.9976 | 1.0010 | 29.0230 |
| warm/date_toordinal | interp | 0.9881 | 0.9809 | 0.9967 | 25.9059 |
| warm/date_weekday | jit | 1.0038 | 0.9979 | 1.0000 | 42.1156 |
| warm/date_weekday | interp | 1.0083 | 0.9894 | 0.9956 | 38.5118 |
| warm/datetime_add | jit | 1.0064 | 1.0154 | 1.0015 | 441.8066 |
| warm/datetime_add | interp | 0.9989 | 1.0041 | 0.9944 | 450.0385 |
| warm/datetime_subtract | jit | 0.9941 | 0.9924 | 1.0010 | 75.0554 |
| warm/datetime_subtract | interp | 1.0068 | 1.0011 | 0.9945 | 73.3800 |
| cold/rich_every_0 | jit | 0.9925 | 1.1417 | 1.0033 | 14.7509 |
| cold/rich_every_0 | interp | 0.9938 | 1.0049 | 0.9940 | 11.2693 |
| cold/rich_every_100 | jit | 0.9951 | 1.0645 | 0.9945 | 12.5825 |
| cold/rich_every_100 | interp | 1.0114 | 0.9957 | 0.9928 | 10.3426 |
| cold/rich_every_2 | jit | 1.0036 | 1.0402 | 0.9995 | 12.4074 |
| cold/rich_every_2 | interp | 1.0113 | 1.0062 | 0.9928 | 10.3994 |
| cold/rich_every_1 | jit | 1.0006 | 1.0243 | 1.0033 | 14.7225 |
| cold/rich_every_1 | interp | 1.0028 | 0.9882 | 0.9940 | 11.8972 |

All paired samples, ranges, regressions, workload/process wall and CPU, RSS,
and cache observations remain. Four cold controls have process CPU but no
separate workload CPU timer. The persistent complex-callback regression must
not be hidden by the successful simple integer/branch controls.

The full suite uses ed99 as baseline, with 24 unchanged fixtures at five pairs
and nine startup/import controls at 31 pairs. The startup helper disables JIT.
Both strict timing gates qualified; all later load, VM, and swap observations
remain. No outcome-based filtering or retry was performed.

The JIT 23-workload mean is 1.002397 of ed99 and
3.384972 of CPython, with 6 time wins.
The historical 21-fixture mean is 2.086925 of CPython,
or 2.090158 using ratios of medians.

All-24 process wall/CPU/RSS ratios to ed99 are 1.008378,
1.006838, and 1.000483.
Peak RSS is 2.060472 of CPython with 0 wins.

Keep 21/23/24 cohorts distinct and use same-run pairs for attribution. No
universal, energy, controlled build-time, portability, or native parallel
performance superiority is established. Large raw text is compressed losslessly;
both stored and uncompressed hashes/sizes are indexed. Executables, frozen
caches, and unselected temporary files remain local. The separate thin-owner
prototype evidence is in ../thin-owner-prototypes/REPORT.md; no runtime memory
layout change is included in this comparison candidate.
