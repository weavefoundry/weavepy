# Guard opaque integer comparisons at the comparison instruction

The overall performance goal remains unachieved. This candidate is under refinement.

Candidate e709418a uses the existing exact-Int guard for one opaque operand
beside an integral peer. It guards at COMPARE_OP after producing calls and
retains both original operands for misses. Bool/float/large integer/subclass
and rich-comparison surprises resume in the interpreter. Two opaque operands
and opaque/float pairs keep their earlier analysis. No call fusion, runtime
helper, object field, cache, unsafe code, or execution policy is added.
The executable is 44,289,536 bytes, 16 bytes larger than immediate ed99.
Earlier intern and native-entry changes and their regressions remain included.

The first analyzer fixture failed TypeUnknown because its opaque producer
argument had no parameter-lane probe. The fixture was corrected to supply
its runtime-equivalent Int lane; the runtime draft was unchanged. Original
failing output, methods, and source hashes remain. The corrected continuation
uses separate logs and manifests. All 59 JIT tests, 342 VM tests, formatting,
Clippy, and no-default-feature checks pass. Observed release build time was
4m08s, which is not a controlled build-time metric.

All 72 targeted release checks and 275 standard compatibility checks pass.
All six extra comparison/rich-comparison/long-integer/call/unittest suites
select nonempty tests and pass. All 19 named comparison functions actually
compile and rebuild continuations with exact CPython results. The regression
covers all operators and orders, arbitrary rich-comparison results, reflected
dispatch, truth conversion, integer boundaries, subclasses, producer and
comparison exceptions, callback order, loop locals and exact traceback line,
tracing, and recovery. Baseline v1/v2 checks remain separate.

Untimed datetime results agree with CPython; compiled functions rise 26 to 28
(__lt__ and __gt__), but native-call deopts rise from 0 to 64 in this driver.
_ymd2ord still rejects MixedArithTypes; field validators advance to a formatting
operand-lane rejection. These observations do not establish speed gains.
A separate untimed a1e/ed99 nested-loop follow-up found identical native stats
and zero deopts. It does not explain or erase the earlier 13.4 percent regression.

Focused timing uses 15 warm and four cold controls at work 5000 and seven pairs
of candidate, immediate ed99, retained 539c, and CPython. All WeavePy builds
are measured in both modes. GIL-disabled runs are correctness controls only.
All values and per-binary frozen caches agree. Lower ratios are better.

| Case | Mode | Time / predecessor | RSS / predecessor | Time / CPython |
| --- | --- | ---: | ---: | ---: |
| warm/left_int | jit | 0.7868 | 1.0005 | 7.3810 |
| warm/left_int | interp | 1.0003 | 0.9994 | 10.3865 |
| warm/right_int | jit | 0.7919 | 1.0027 | 7.7464 |
| warm/right_int | interp | 1.0005 | 0.9971 | 11.0420 |
| warm/branch_int | jit | 0.3645 | 0.9989 | 4.3764 |
| warm/branch_int | interp | 0.9883 | 1.0006 | 11.0749 |
| warm/bool_miss | jit | 0.9947 | 1.0022 | 11.6201 |
| warm/bool_miss | interp | 0.9863 | 0.9971 | 10.8283 |
| warm/float_miss | jit | 0.9897 | 1.0016 | 11.9911 |
| warm/float_miss | interp | 0.9874 | 0.9988 | 10.7407 |
| warm/bigint_miss | jit | 0.9998 | 1.0043 | 12.4420 |
| warm/bigint_miss | interp | 1.0020 | 0.9982 | 12.4970 |
| warm/rich_every_0 | jit | 1.1255 | 1.0038 | 12.3189 |
| warm/rich_every_0 | interp | 1.0049 | 0.9982 | 11.1221 |
| warm/rich_every_100 | jit | 1.0318 | 1.0054 | 10.8323 |
| warm/rich_every_100 | interp | 0.9884 | 0.9988 | 10.4841 |
| warm/rich_every_2 | jit | 0.9938 | 1.0081 | 10.7888 |
| warm/rich_every_2 | interp | 0.9905 | 0.9976 | 10.2054 |
| warm/rich_every_1 | jit | 1.0042 | 1.0000 | 13.1975 |
| warm/rich_every_1 | interp | 0.9837 | 0.9965 | 11.9697 |
| warm/miss_burst_recovery | jit | 1.0069 | 1.0005 | 9.9175 |
| warm/miss_burst_recovery | interp | 1.0053 | 0.9953 | 11.1136 |
| warm/date_toordinal | jit | 0.9944 | 0.9995 | 27.2955 |
| warm/date_toordinal | interp | 0.9980 | 0.9934 | 23.9591 |
| warm/date_weekday | jit | 0.9958 | 1.0010 | 43.6132 |
| warm/date_weekday | interp | 0.9913 | 0.9978 | 41.0436 |
| warm/datetime_add | jit | 0.9836 | 1.0015 | 449.1025 |
| warm/datetime_add | interp | 1.0066 | 0.9978 | 454.7836 |
| warm/datetime_subtract | jit | 0.9968 | 1.0010 | 75.2127 |
| warm/datetime_subtract | interp | 0.9964 | 0.9989 | 72.5543 |
| cold/rich_every_0 | jit | 1.1510 | 1.0033 | 14.7212 |
| cold/rich_every_0 | interp | 0.9945 | 0.9970 | 11.0699 |
| cold/rich_every_100 | jit | 1.0967 | 1.0049 | 12.7809 |
| cold/rich_every_100 | interp | 0.9920 | 0.9988 | 10.4977 |
| cold/rich_every_2 | jit | 1.0299 | 1.0050 | 12.2777 |
| cold/rich_every_2 | interp | 0.9962 | 0.9946 | 10.2128 |
| cold/rich_every_1 | jit | 1.0095 | 1.0000 | 14.2699 |
| cold/rich_every_1 | interp | 0.9896 | 0.9982 | 11.8234 |

Every sample, paired range, regression, workload/process wall and CPU metric,
peak RSS, and cache observation remains. Four cold controls have process CPU
but no separate workload CPU timer. Native-coverage counts are not speed data.

The full suite retains 24 unchanged fixtures at five pairs and nine startup/
import controls at 31 pairs, with immediate ed99 as the baseline. The startup
helper disables JIT. Both timing gates qualified; all later load, VM, and swap
observations remain. Samples are never selected or retried by outcome.

The 23-workload JIT mean is 1.002727 of its predecessor and
3.387763 of CPython, with 6 time wins.
The historical 21-fixture mean is 2.091439 of CPython,
or 2.091854 using ratios of medians.

All-24 process wall/CPU/RSS ratios to its predecessor are 1.008230,
1.006883, and 1.001823.
Peak RSS is 2.062056 of CPython with 0 wins.

Keep 21/23/24 cohorts distinct and use same-run pairs for attribution. No
universal, energy, controlled build-time, portability, or native parallel
performance superiority is established. Large raw text is compressed losslessly;
the index verifies both stored and uncompressed sizes and hashes. Executables,
frozen caches, and unselected temporary files remain local.
