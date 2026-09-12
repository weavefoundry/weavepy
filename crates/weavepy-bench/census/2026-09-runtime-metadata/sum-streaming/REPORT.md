# Stream sums without copying containers

Exact integer lists and tuples now accumulate into a checked 128-bit total while borrowing their contents, returning an inline integer or a larger Python integer as needed. Exact booleans are admitted as elements; noninteger elements and noninteger starts retain generic arithmetic. The tentative total is discarded before fallback. No Python callback runs under a container borrow. The VM and static builtin share this path.

Generic VM sums stream the live iterator instead of first copying the iterable. A callback can append or clear list contents, a failed addition stops consumption immediately, and sum no longer requests a source length hint. The existing generator-specific iteration and StopIteration observer behavior remain intact. No new unsafe code, public Rust API, or object layout is introduced.

Candidate SHA-256: `7ef09c5d3e724632e034bec5c4c3a8cd7a8780beac258bff59a96c09e1db4ae0`; 44,211,280 bytes, 128 bytes above the preceding range release. All 12 measured layouts are unchanged. Frozen inputs contain 118 sources and 37 measurement inputs.

All 318 VM tests, 52 JIT tests, 256 configured compatibility checks, Clippy, and workspace/all-feature/no-JIT checks pass. Two focused units cover the direct integer path, overflow and cancellation, fallback types, callbacks, and prompt finalization. The new regression passes on CPython and fails on the preceding release because a callback-appended element is omitted, producing 6 instead of 10. That original failure is retained.

The 47-case release oracle matches CPython on 42 rows in native, interpreted, and GIL-disabled modes. Four iterator-order differences are repaired. Three existing TypeError wording differences and two existing compensated-float differences remain exactly as recorded before the change; invalid arities still raise TypeError. In the two floating-point probes, CPython returns 1.0 and WeavePy returns 0.0. This integer optimization does not establish floating-point accuracy parity. Finalizer timing and five first-call/warmed benchmark return-value checks match CPython.

A separate untimed RSS diagnostic reads the operating-system process high-water mark before construction, after endpoint checks, and after sum. For a two-million-element list, the preceding release rises from about 76 MB after construction to 124 MB after sum; the candidate stays near 76 MB, while CPython stays near 95 MB. Tuple construction already peaks around 124 MB in WeavePy and 114 MB in CPython. These single-run diagnostics locate the peak; the paired population measurements below retain the complete workload, including its checksum.

For the complete two-million-integer list workload, process elapsed time is about 40 percent below CPython and peak RSS about 20 percent below it. Exact list and tuple sum timers take about 2.8 percent of the preceding release time. Generic float, mixed, and large-integer fallback timers regress 13 to 20 percent; the range sum timer regresses 8.4 percent. Those remaining regressions are targets for further work, and all samples are retained.

Focused groups use seven alternating paired cycles after a discarded cycle. Their preceding reference is the e3d4 exact-range release. Ratios below one mean lower time or memory. Process elapsed time, CPU, and peak RSS include construction, startup, and validation. Sum timers exclude input construction; population timers cover construction but exclude their later checksum. The custom groups use interpreter mode, and floating-point controls use exactly representable half-integer increments. They are performance controls, not floating-point accuracy proofs. Tiny sum timers include timer and call overhead. Cold controls omit explicit workload warmup; operating-system caches are not flushed.

## cold

| Fixture | Time/previous | Wall/previous | CPU/previous | RSS/previous | Time/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|---:|
| str_methods | 0.9971 | 0.9820 | 0.9848 | 0.9975 | 2.0164 | 2.1987 |
| list_ops | 1.0029 | 1.0029 | 1.0023 | 1.0011 | 13.7782 | 1.9817 |
| attr_access | 0.9961 | 0.9932 | 0.9947 | 1.0021 | 3.1033 | 2.0287 |
| call_overhead | 0.9991 | 0.9972 | 0.9975 | 0.9984 | 9.1317 | 2.0451 |
| jitkernels | 0.9859 | 0.9828 | 0.9859 | 0.9984 | 0.8758 | 1.9883 |

## warm

| Fixture | Time/previous | Wall/previous | CPU/previous | RSS/previous | Time/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|---:|
| str_methods | 0.9906 | 0.9952 | 0.9958 | 1.0000 | 1.9343 | 2.1572 |
| list_ops | 0.9848 | 0.9882 | 0.9885 | 1.0054 | 13.5212 | 1.9552 |
| attr_access | 0.9909 | 0.9855 | 0.9870 | 1.0021 | 3.1296 | 2.0000 |
| call_overhead | 0.9939 | 1.0045 | 1.0051 | 0.9995 | 9.1889 | 2.0178 |
| jitkernels | 0.9777 | 0.9815 | 0.9846 | 0.9984 | 0.8347 | 1.9568 |

## sums

| Fixture | Time/previous | Wall/previous | CPU/previous | RSS/previous | Time/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|---:|
| list_ints_32 | 0.8068 | 1.0076 | 1.0093 | 1.0029 | 1.3793 | 1.8602 |
| list_ints_10000 | 0.0311 | 0.9688 | 0.9741 | 0.9965 | 0.3586 | 1.8070 |
| list_ints_1000000 | 0.0259 | 0.4602 | 0.4460 | 0.6860 | 0.2722 | 0.9430 |
| list_ints_2000000 | 0.0276 | 0.3227 | 0.3095 | 0.6143 | 0.2949 | 0.7988 |
| list_bools | 0.0183 | 0.7954 | 0.7874 | 1.0043 | 0.3263 | 2.0710 |
| list_overflow | 0.0051 | 0.6573 | 0.6440 | 1.0055 | 0.0518 | 1.9882 |
| list_floats | 1.1774 | 0.9995 | 1.0051 | 1.0055 | 15.4236 | 1.9843 |
| list_mixed | 1.1664 | 1.0098 | 1.0172 | 1.0065 | 18.2998 | 1.9862 |
| list_big_last | 1.2022 | 1.0102 | 1.0131 | 1.0050 | 20.6285 | 1.9921 |
| tuple_ints_32 | 0.7972 | 0.9695 | 0.9596 | 1.0053 | 1.4166 | 1.8596 |
| tuple_ints_10000 | 0.0339 | 0.9823 | 0.9729 | 1.0029 | 0.4352 | 1.8204 |
| tuple_ints_1000000 | 0.0267 | 0.4995 | 0.4844 | 1.0022 | 0.3171 | 1.1673 |
| tuple_ints_2000000 | 0.0278 | 0.3788 | 0.3658 | 1.0013 | 0.3341 | 1.0946 |
| tuple_bools | 0.0183 | 0.8064 | 0.8029 | 1.0039 | 0.3126 | 2.3327 |
| tuple_overflow | 0.0047 | 0.6741 | 0.6581 | 1.0060 | 0.0475 | 2.1327 |
| tuple_floats | 1.1338 | 0.9979 | 1.0057 | 1.0033 | 13.1867 | 2.1320 |
| tuple_mixed | 1.1615 | 1.0093 | 1.0023 | 1.0074 | 19.7353 | 2.1329 |
| tuple_big_last | 1.1586 | 1.0165 | 1.0201 | 1.0074 | 20.1256 | 2.1377 |
| range_2000000 | 1.0845 | 1.0513 | 1.0543 | 0.3700 | 7.6130 | 1.8595 |
| generator_100000 | 0.9977 | 0.9932 | 0.9949 | 1.0047 | 10.1733 | 1.8632 |

## populations

| Fixture | Time/previous | Wall/previous | CPU/previous | RSS/previous | Time/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|---:|
| list_range_10000 | 0.8655 | 0.9881 | 0.9870 | 0.9994 | 0.2173 | 1.8201 |
| list_range_100000 | 0.9628 | 0.8663 | 0.8605 | 0.9297 | 0.3238 | 1.6100 |
| list_range_500000 | 1.0309 | 0.6244 | 0.6137 | 0.7735 | 0.3417 | 1.1437 |
| list_range_1000000 | 0.9902 | 0.4606 | 0.4470 | 0.6863 | 0.3164 | 0.9440 |
| list_range_2000000 | 1.0058 | 0.3274 | 0.3156 | 0.6145 | 0.3173 | 0.7984 |
| tuple_range_10000 | 0.9900 | 0.9670 | 0.9547 | 1.0064 | 0.4235 | 1.8259 |
| tuple_range_100000 | 1.0206 | 0.8878 | 0.8850 | 1.0045 | 0.5700 | 1.6587 |
| tuple_range_500000 | 1.0161 | 0.6384 | 0.6256 | 1.0038 | 0.5702 | 1.2640 |
| tuple_range_1000000 | 0.9601 | 0.5026 | 0.4918 | 1.0030 | 0.5544 | 1.1673 |
| tuple_range_2000000 | 0.9953 | 0.3898 | 0.3754 | 1.0016 | 0.5893 | 1.0955 |
| list_repeated_10000 | 1.0075 | 0.9479 | 0.9607 | 0.9994 | 2.6813 | 1.8568 |
| list_repeated_100000 | 1.0016 | 0.8597 | 0.8574 | 0.9321 | 5.4944 | 1.9378 |
| list_repeated_500000 | 0.9703 | 0.6355 | 0.6151 | 0.7740 | 4.8703 | 2.1026 |
| list_repeated_1000000 | 0.9951 | 0.4733 | 0.4558 | 0.6866 | 4.6313 | 2.2537 |
| list_repeated_2000000 | 1.0069 | 0.3436 | 0.3363 | 0.6145 | 4.6712 | 2.4461 |
| tuple_repeated_10000 | 1.0219 | 1.0185 | 1.0160 | 1.0075 | 5.6811 | 1.8731 |
| tuple_repeated_100000 | 0.9719 | 0.8606 | 0.8516 | 1.0055 | 8.4022 | 2.0805 |
| tuple_repeated_500000 | 1.0270 | 0.6312 | 0.6189 | 1.0028 | 8.3271 | 2.7210 |
| tuple_repeated_1000000 | 1.0053 | 0.5079 | 0.5040 | 1.0024 | 8.7311 | 3.2977 |
| tuple_repeated_2000000 | 1.0047 | 0.3966 | 0.3829 | 1.0020 | 8.6190 | 3.9874 |

## Full census

The full 24-fixture refresh uses five alternating paired cycles after a discarded cycle. Its base is checkpoint 9a69c41 and its previous reference is the 76d0 string argument release, the preceding complete census. This differs from the focused preceding reference. Aggregates are geometric means of per-fixture median paired ratios. Workload time excludes the startup fixture, leaving 23 timers; process elapsed time, CPU, and peak RSS include all 24.

| Metric | JIT/checkpoint | Interpreter/checkpoint | JIT/string args | Interpreter/string args | JIT/CPython | Interpreter/CPython |
|---|---:|---:|---:|---:|---:|---:|
| ns | 0.8167 | 0.9607 | 0.9925 | 0.9922 | 3.5510 | 9.5627 |
| wall_ns | 0.8813 | 0.9720 | 0.9858 | 0.9952 | 3.3757 | 5.8684 |
| cpu_ns | 0.8873 | 0.9721 | 0.9883 | 0.9963 | 3.4568 | 6.0747 |
| rss_bytes | 0.9472 | 0.9440 | 0.9938 | 1.0032 | 2.0965 | 1.9355 |

JIT workload-time wins against CPython: 6/23. Peak-RSS wins: 0/24. The objective of superiority across every meaningful metric remains unachieved.

| Fixture | Time/string args | CPU/string args | RSS/string args | Time/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|
| fannkuch | 0.8730 | 0.8902 | 1.0000 | 9.0121 | 1.9871 |
| nbody | 0.9802 | 0.9839 | 0.9995 | 8.6538 | 1.9757 |
| fib | 0.9979 | 0.9884 | 0.9995 | 3.0090 | 2.0065 |
| pidigits | 1.0043 | 1.0020 | 0.9959 | 0.8834 | 1.9877 |
| pyaes | 1.0025 | 0.9957 | 1.0000 | 0.6525 | 1.9905 |
| richards | 0.9854 | 0.9823 | 1.0027 | 8.3301 | 2.0011 |
| sumvm | 0.9991 | 0.9771 | 0.9968 | 0.0564 | 2.0043 |
| nested_loops | 1.0165 | 0.9941 | 0.9979 | 0.0839 | 2.0043 |
| jitloop | 1.0005 | 0.9902 | 1.0000 | 0.0738 | 2.0184 |
| jitkernels | 0.9852 | 0.9812 | 0.9989 | 0.8570 | 1.9830 |
| deltablue | 0.9979 | 0.9962 | 0.9960 | 19.7531 | 2.1631 |
| float_math | 1.0126 | 1.0146 | 0.9995 | 7.6221 | 2.9968 |
| spectral_norm | 0.9911 | 0.9877 | 1.0027 | 2.1100 | 2.0150 |
| json_bench | 0.9991 | 0.9667 | 0.9380 | 1.1597 | 2.5654 |
| str_methods | 1.0029 | 0.9989 | 0.9976 | 2.0110 | 2.1935 |
| dict_ops | 0.9971 | 0.9999 | 1.0022 | 5.5433 | 1.9797 |
| list_ops | 0.9966 | 0.9964 | 1.0000 | 13.8759 | 1.9796 |
| attr_access | 0.9875 | 0.9841 | 1.0000 | 3.1339 | 2.0387 |
| call_overhead | 1.0097 | 1.0081 | 0.9979 | 9.2126 | 2.0482 |
| generators | 1.0011 | 1.0012 | 1.0005 | 9.7021 | 2.0065 |
| deque_ops | 0.9971 | 0.9951 | 0.9990 | 16.7453 | 2.0235 |
| datetime_ops | 0.9927 | 0.9927 | 0.9980 | 152.2844 | 2.1563 |
| pickle_bench | 1.0075 | 1.0058 | 0.9348 | 331.1225 | 2.4703 |
| startup | 0.9890 | 0.9927 | 0.9978 | 1.4734 | 1.9924 |

Every gain, regression, and raw sample is retained. Actual process RSS determines memory comparisons; sys.getsizeof and tracemalloc are not physical-memory measurements. The illustrative fannkuch fixture is not canonical fannkuch, and pyaes is an XOR scrambler. The sum measurements ran outside the tool filesystem sandbox (require_escalated); the preceding string-argument full census ran under the default sandbox. Controlled default/outside/default probes, each with seven samples, reproduce CPython datetime medians of 250.688, 25.133, and 247.635 ms with identical executable and fixture hashes and captured timezone/locale/Python environment. Timezone variants retain the launch-mode effect. This explains the reference shift between these two latest censuses, without assigning a mechanism or retroactively identifying every earlier variation. Within-run paired comparisons share the launch condition. CPython-relative ratios across these stages are not measurements under identical conditions. The probe controllers, raw samples, and launch comparison are retained; future runs record their declared execution context.

Dedicated startup/import variants, parallel throughput, and explicit GC-pause distributions are not repeated here. CPython is the recorded 3.14.7 GIL build; native execution remains gated in WeavePy GIL-disabled mode. No controlled build-latency, energy, or free-threaded CPython claim is made.
