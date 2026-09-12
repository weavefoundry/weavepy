# Exact numeric sequence sums

A second numeric path handles exact lists and tuples of integers, booleans, large integers, and floats. A primitive-type prepass ensures no Python callback runs under the container borrow. The existing tight integer-only loop stays first. Other types retain the live iterator path. No new unsafe code, public Rust API, or object layout is introduced.

Checked integer accumulation preserves the one-way transition into floating-point or generic arithmetic. Integer overflow, large starts, and boolean starts preserve their generic behavior. Compensated float addition preserves operation order, signed zero, infinities, NaNs, and integer conversion errors. The implementation was checked against [CPython 3.14.7](https://raw.githubusercontent.com/python/cpython/v3.14.7/Python/bltinmodule.c). This stage does not add compensated arithmetic to generators, complex reductions, or subclass callback paths.

Candidate SHA-256: `f655a1bdaeaaa26cfaaddb3fce7ad358586fe3453d3283e0e98216d976899b30`; 44,211,792 bytes. The preceding reference is the streaming-sum release, SHA-256 `7ef09c5d3e724632e034bec5c4c3a8cd7a8780beac258bff59a96c09e1db4ae0`. All 12 measured layouts are unchanged. Frozen inputs contain 119 sources and 37 measurement inputs.

All 319 VM tests, 52 JIT tests, 259 configured compatibility checks, Clippy, and workspace/all-feature/no-JIT checks pass. The permanent numeric fixture covers 185 value/start combinations in both list and tuple form. Original failures and the expanded release oracle are retained. Five independent first-call/warmed benchmark return checks match CPython in native, interpreted, and GIL-disabled modes.

The 764-case numeric oracle fixes 36 previously differing cases per execution mode and retains 46 existing CPython differences. No unexpected differences appear. It includes iterator/generator forms, complex values, subclasses, callback order, integer overflow recovery, special floats, exact float encodings, and deterministic randomized cases. Remaining differences stay visible in the raw report; they aren't accuracy-parity claims.

The original 47-case sum oracle is also repeated. Its two compensated-float differences are repaired; three existing error wording differences remain. Invalid argument cases still raise TypeError.

The initial four focused groups use seven alternating paired cycles after a discarded cycle. Because integer and attribute controls showed regressions, all 20 sum cases and cold/warm attribute controls were repeated for 15 paired cycles; both runs remain available. All runs take place outside the tool filesystem sandbox; each report records that declared launch context and its captured timezone/locale/Python environment. The label is caller-declared, not automatic sandbox detection. This condition matches the preceding streaming-sum measurements. Every gain, regression, and sample is retained.

The 15-cycle repeat reduces the six float/mixed/large-integer sum timers to 3.4 to 3.9 percent of the preceding release, and 56 to 79 percent of CPython. These are workload-timer wins: their complete processes still take 1.37 to 1.45 times CPython elapsed time and use about 1.97 to 2.12 times its peak RSS. The large two-million-integer list population still takes about 40 percent less elapsed time and 20 percent less peak RSS than CPython.

The repeat retains a 4.4 percent overflow-list sum slowdown, a 2.8 percent range sum slowdown, and attribute slowdowns of 1.8 percent cold and 1.5 percent warm. Earlier tuple-sum slowdowns do not repeat: the two-million-element tuple timer changes from a 7.2 percent initial slowdown to a 5.5 percent repeat improvement. Both measurements remain available; the change does not improve every control. The initial population run also retains a 4.0 percent slowdown in the two-million-element list construction timer, with whole-process elapsed time about 0.9 percent lower.

Sum timers exclude input construction; population timers cover construction but exclude the later checksum. Process elapsed time, CPU, and peak RSS include startup, construction, and validation. Custom groups use interpreter mode. Cold controls omit explicit workload warmup; operating-system caches are not flushed. Floating-point speed controls use exactly representable half-integer increments; the separate oracle checks accuracy.

## cold

| Fixture | Time/previous | Wall/previous | CPU/previous | RSS/previous | Time/CPython | Wall/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|---:|---:|
| str_methods | 1.0002 | 1.0029 | 1.0015 | 0.9936 | 2.0391 | 1.8120 | 2.1816 |
| list_ops | 1.0028 | 1.0020 | 1.0011 | 0.9951 | 13.7360 | 8.0150 | 1.9742 |
| attr_access | 1.0094 | 1.0113 | 1.0117 | 0.9874 | 3.1795 | 2.4772 | 2.0205 |
| call_overhead | 1.0026 | 1.0014 | 1.0020 | 0.9895 | 8.9508 | 6.6404 | 2.0333 |
| jitkernels | 1.0003 | 0.9965 | 0.9952 | 0.9930 | 0.8721 | 1.1122 | 1.9808 |

## warm

| Fixture | Time/previous | Wall/previous | CPU/previous | RSS/previous | Time/CPython | Wall/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|---:|---:|
| str_methods | 1.0054 | 1.0003 | 0.9993 | 0.9889 | 1.9503 | 1.8482 | 2.1379 |
| list_ops | 0.9992 | 0.9999 | 0.9998 | 0.9930 | 13.7584 | 9.7276 | 1.9393 |
| attr_access | 1.0221 | 1.0170 | 1.0131 | 0.9907 | 3.0965 | 2.6354 | 1.9833 |
| call_overhead | 1.0036 | 1.0009 | 1.0009 | 0.9922 | 8.9391 | 7.5098 | 1.9958 |
| jitkernels | 1.0021 | 1.0037 | 1.0072 | 0.9921 | 0.8389 | 1.0116 | 1.9411 |

## sums

| Fixture | Time/previous | Wall/previous | CPU/previous | RSS/previous | Time/CPython | Wall/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|---:|---:|
| list_ints_32 | 0.9766 | 1.0087 | 1.0061 | 0.9936 | 1.2526 | 1.4013 | 1.8420 |
| list_ints_10000 | 0.9758 | 0.9806 | 0.9961 | 0.9930 | 0.3381 | 1.4094 | 1.7994 |
| list_ints_1000000 | 0.9730 | 0.9809 | 0.9855 | 0.9962 | 0.2713 | 0.7942 | 0.9392 |
| list_ints_2000000 | 0.9875 | 0.9895 | 0.9980 | 0.9976 | 0.2461 | 0.6157 | 0.7967 |
| list_bools | 0.9824 | 0.9948 | 0.9960 | 0.9944 | 0.3236 | 1.4206 | 2.0638 |
| list_overflow | 1.0920 | 0.9998 | 0.9997 | 0.9925 | 0.0520 | 1.2984 | 1.9694 |
| list_floats | 0.0338 | 0.8850 | 0.8734 | 0.9940 | 0.5620 | 1.3756 | 1.9666 |
| list_mixed | 0.0371 | 0.8907 | 0.8782 | 0.9940 | 0.7575 | 1.4196 | 1.9763 |
| list_big_last | 0.0373 | 0.8641 | 0.8596 | 0.9926 | 0.7519 | 1.4275 | 1.9734 |
| tuple_ints_32 | 1.0087 | 1.0010 | 1.0095 | 0.9942 | 1.2765 | 1.4135 | 1.8494 |
| tuple_ints_10000 | 1.0468 | 1.0085 | 1.0114 | 0.9931 | 0.4274 | 1.4002 | 1.8093 |
| tuple_ints_1000000 | 1.0159 | 0.9845 | 0.9939 | 0.9972 | 0.3210 | 0.8780 | 1.1628 |
| tuple_ints_2000000 | 1.0717 | 0.9944 | 0.9986 | 0.9985 | 0.3484 | 0.7120 | 1.0933 |
| tuple_bools | 1.0051 | 0.9938 | 0.9919 | 0.9950 | 0.3215 | 1.4694 | 2.3238 |
| tuple_overflow | 0.9912 | 0.9829 | 0.9817 | 0.9917 | 0.0494 | 1.3256 | 2.1172 |
| tuple_floats | 0.0335 | 0.8830 | 0.8766 | 0.9949 | 0.5649 | 1.4052 | 2.1219 |
| tuple_mixed | 0.0402 | 0.8625 | 0.8923 | 0.9935 | 0.7968 | 1.4555 | 2.1223 |
| tuple_big_last | 0.0398 | 0.8800 | 0.8870 | 0.9931 | 0.7537 | 1.4430 | 2.1234 |
| range_2000000 | 1.0273 | 1.0188 | 1.0185 | 0.9895 | 7.6983 | 3.4924 | 1.8431 |
| generator_100000 | 1.0073 | 1.0041 | 1.0040 | 0.9942 | 10.3147 | 2.0670 | 1.8564 |

## populations

| Fixture | Time/previous | Wall/previous | CPU/previous | RSS/previous | Time/CPython | Wall/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|---:|---:|
| list_range_10000 | 1.0468 | 0.9990 | 1.0001 | 0.9907 | 0.2492 | 1.3752 | 1.7989 |
| list_range_100000 | 0.9955 | 0.9826 | 0.9864 | 0.9920 | 0.3239 | 1.2856 | 1.5924 |
| list_range_500000 | 1.0079 | 1.0158 | 1.0224 | 0.9947 | 0.3194 | 1.0140 | 1.1358 |
| list_range_1000000 | 1.0044 | 0.9892 | 0.9753 | 0.9944 | 0.3238 | 0.8101 | 0.9396 |
| list_range_2000000 | 1.0397 | 0.9909 | 0.9922 | 0.9970 | 0.3324 | 0.6055 | 0.7966 |
| tuple_range_10000 | 1.0431 | 0.9948 | 0.9993 | 0.9925 | 0.4434 | 1.3826 | 1.8071 |
| tuple_range_100000 | 1.0534 | 1.0141 | 1.0196 | 0.9936 | 0.6025 | 1.3267 | 1.6417 |
| tuple_range_500000 | 0.9658 | 0.9966 | 0.9968 | 0.9962 | 0.5736 | 1.0361 | 1.2610 |
| tuple_range_1000000 | 1.0221 | 1.0125 | 1.0181 | 0.9968 | 0.5991 | 0.8787 | 1.1637 |
| tuple_range_2000000 | 0.9926 | 0.9987 | 1.0017 | 0.9980 | 0.5840 | 0.7246 | 1.0929 |
| list_repeated_10000 | 1.0000 | 0.9839 | 0.9855 | 0.9919 | 2.5916 | 1.4045 | 1.8443 |
| list_repeated_100000 | 1.0149 | 0.9884 | 1.0025 | 0.9909 | 5.2232 | 1.3923 | 1.9191 |
| list_repeated_500000 | 0.9880 | 0.9739 | 0.9768 | 0.9955 | 4.6735 | 1.3509 | 2.0862 |
| list_repeated_1000000 | 1.0246 | 1.0015 | 1.0003 | 0.9962 | 4.7716 | 1.3206 | 2.2456 |
| list_repeated_2000000 | 1.0151 | 0.9939 | 0.9995 | 0.9972 | 4.7329 | 1.2886 | 2.4388 |
| tuple_repeated_10000 | 1.0132 | 1.0030 | 1.0051 | 0.9931 | 5.1334 | 1.3942 | 1.8536 |
| tuple_repeated_100000 | 1.0091 | 0.9779 | 0.9870 | 0.9936 | 8.1592 | 1.4050 | 2.0754 |
| tuple_repeated_500000 | 0.9873 | 0.9932 | 0.9945 | 0.9972 | 8.2237 | 1.3947 | 2.7224 |
| tuple_repeated_1000000 | 1.0222 | 1.0034 | 1.0032 | 0.9974 | 8.3073 | 1.4078 | 3.2817 |
| tuple_repeated_2000000 | 1.0123 | 0.9999 | 0.9987 | 0.9980 | 8.6345 | 1.4628 | 3.9826 |

## sums-repeat

| Fixture | Time/previous | Wall/previous | CPU/previous | RSS/previous | Time/CPython | Wall/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|---:|---:|
| list_ints_32 | 1.0541 | 1.0001 | 1.0054 | 0.9924 | 1.2771 | 1.4116 | 1.8489 |
| list_ints_10000 | 0.9962 | 0.9997 | 1.0119 | 0.9901 | 0.3536 | 1.3909 | 1.8017 |
| list_ints_1000000 | 1.0503 | 0.9957 | 0.9891 | 0.9956 | 0.2892 | 0.8051 | 0.9398 |
| list_ints_2000000 | 1.0072 | 0.9954 | 0.9984 | 0.9972 | 0.2774 | 0.6127 | 0.7964 |
| list_bools | 0.9980 | 0.9971 | 0.9966 | 0.9944 | 0.3354 | 1.4186 | 2.0547 |
| list_overflow | 1.0438 | 0.9909 | 0.9898 | 0.9941 | 0.0478 | 1.2851 | 1.9744 |
| list_floats | 0.0342 | 0.8722 | 0.8733 | 0.9935 | 0.5617 | 1.3718 | 1.9724 |
| list_mixed | 0.0360 | 0.8732 | 0.8630 | 0.9930 | 0.7344 | 1.4301 | 1.9705 |
| list_big_last | 0.0369 | 0.8829 | 0.8868 | 0.9936 | 0.7576 | 1.4240 | 1.9764 |
| tuple_ints_32 | 0.9364 | 1.0052 | 1.0122 | 0.9936 | 1.2353 | 1.3755 | 1.8442 |
| tuple_ints_10000 | 0.9810 | 1.0087 | 1.0180 | 0.9925 | 0.4188 | 1.3857 | 1.8061 |
| tuple_ints_1000000 | 1.0236 | 0.9830 | 0.9911 | 0.9976 | 0.3018 | 0.8682 | 1.1637 |
| tuple_ints_2000000 | 0.9455 | 0.9973 | 0.9990 | 0.9984 | 0.3179 | 0.7152 | 1.0932 |
| tuple_bools | 0.9805 | 0.9893 | 0.9923 | 0.9946 | 0.3260 | 1.4630 | 2.3232 |
| tuple_overflow | 0.9629 | 0.9902 | 1.0042 | 0.9949 | 0.0488 | 1.3275 | 2.1224 |
| tuple_floats | 0.0361 | 0.8831 | 0.8866 | 0.9945 | 0.5953 | 1.4074 | 2.1204 |
| tuple_mixed | 0.0387 | 0.8993 | 0.9132 | 0.9949 | 0.7883 | 1.4527 | 2.1209 |
| tuple_big_last | 0.0383 | 0.8884 | 0.8909 | 0.9940 | 0.7746 | 1.4504 | 2.1180 |
| range_2000000 | 1.0284 | 1.0228 | 1.0255 | 0.9936 | 7.8364 | 3.5082 | 1.8485 |
| generator_100000 | 1.0013 | 1.0017 | 0.9980 | 0.9902 | 10.4577 | 2.0384 | 1.8482 |

## attr-cold-repeat

| Fixture | Time/previous | Wall/previous | CPU/previous | RSS/previous | Time/CPython | Wall/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|---:|---:|
| attr_access | 1.0179 | 1.0105 | 1.0122 | 0.9900 | 3.1913 | 2.4600 | 2.0150 |

## attr-warm-repeat

| Fixture | Time/previous | Wall/previous | CPU/previous | RSS/previous | Time/CPython | Wall/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|---:|---:|
| attr_access | 1.0153 | 1.0145 | 1.0156 | 0.9876 | 3.1191 | 2.6708 | 1.9855 |

The full 24-fixture census is not repeated in this focused stage; the [streaming-sum census](../sum-streaming/REPORT.md) remains the latest full refresh. Dedicated startup/import variants, parallel throughput, and explicit GC-pause distributions are not repeated. Actual OS peak RSS determines memory comparisons. CPython is the recorded 3.14.7 GIL build, and WeavePy native execution remains gated with the GIL disabled. No free-threaded CPython, energy, or controlled build-latency claim is made. The objective of beating CPython across every meaningful metric remains unachieved.
