# Native string argument storage and validation

Common native string calls keep the receiver and up to three explicit arguments in a four-slot stack array. Larger calls retain the heap fallback and builtin error handling. Replace validates argument counts and keywords before conversion, borrows its count argument, rejects None counts, and preserves callback exceptions. The pre-CALL guard for index callbacks and activation cleanup remain intact. No new unsafe code, public Rust API, or helper ABI is introduced.

Candidate SHA-256: `76d06d7a7b0e76779dc79fc8e4bf5dacbe1eb21eed7fec3fb458250568131fb0`; 44,211,008 bytes, unchanged from split/activation cleanup. All 12 measured release-library layouts are unchanged. Frozen inputs contain 116 sources and 37 measurement inputs.

All 313 VM tests, 52 JIT tests, 247 configured compatibility checks, Clippy, and workspace/all-feature/no-JIT checks pass. The original permanent regression failed because the preceding release accepted excess replace arguments. The new regression covers all 35 valid native string methods, invalid arities, keyword precedence, surrogate-containing strings, exactly-once argument construction, no index callback for invalid arity, large counts, and callback exceptions. Five isolated VM cases establish native execution with zero through four explicit arguments, including the heap fallback. The initial proof wrongly required direct calls instead of allowing framed native entries; only its entry-kind assertion changed, and its failure log and source are preserved.

The 58-case release oracle matches CPython on all 35 valid calls and all targeted replace validation cases in native, interpreted, and GIL-disabled modes. The existing excess-split error wording differs; TypeError is still required, and that one wording difference is retained explicitly. The other 57 rows match exactly. The separate five-workload oracle checks first and warmed return values against CPython without including those checks in timing samples.

Focused measurements use nine alternating paired cycles for five cold and five warm workloads, plus five paired cycles at four string work sizes. Each follows a discarded cycle. Here GC-index is 8e86 and previous is ad126 split/activation cleanup. Ratios below one mean lower time or memory. Workload time excludes startup; process CPU and peak RSS include it. Cold means no explicit workload warmup, not flushed operating-system caches.

| Mode | Fixture | Time/GC-index | CPU/GC-index | RSS/GC-index | Time/previous | CPU/previous | RSS/previous | Time/CPython | RSS/CPython |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| cold-1000 | str_methods | 0.7557 | 0.9306 | 1.0523 | 0.9785 | 0.9959 | 1.0005 | 3.3143 | 2.2026 |
| cold-15000 | str_methods | 0.6617 | 0.7287 | 1.0523 | 0.9693 | 0.9830 | 0.9990 | 2.0159 | 2.1942 |
| cold-4000 | str_methods | 0.6872 | 0.8364 | 1.0501 | 0.9760 | 0.9874 | 0.9980 | 2.2976 | 2.1907 |
| cold-60000 | str_methods | 0.6377 | 0.6591 | 1.0537 | 0.9535 | 0.9562 | 0.9985 | 1.9152 | 2.1961 |
| cold | str_methods | 0.6550 | 0.7243 | 1.0536 | 0.9667 | 0.9755 | 0.9975 | 2.0327 | 2.1976 |
| cold | list_ops | 0.9930 | 0.9947 | 1.0011 | 0.9643 | 0.9663 | 0.9984 | 13.8270 | 1.9806 |
| cold | attr_access | 1.0497 | 1.0456 | 0.9610 | 1.0163 | 1.0075 | 1.0016 | 3.2110 | 2.0354 |
| cold | call_overhead | 1.0073 | 1.0089 | 1.0068 | 0.9805 | 0.9798 | 1.0047 | 9.1914 | 2.0591 |
| cold | jitkernels | 1.0084 | 1.0057 | 1.0038 | 1.0007 | 0.9917 | 0.9989 | 0.8768 | 1.9968 |
| warm-1000 | str_methods | 0.8748 | 0.9289 | 0.9190 | 0.9652 | 0.9897 | 1.0024 | 1.9648 | 2.1589 |
| warm-15000 | str_methods | 0.6900 | 0.7035 | 0.5630 | 0.9623 | 0.9680 | 0.9971 | 1.9507 | 2.1613 |
| warm-4000 | str_methods | 0.8532 | 0.8298 | 0.6433 | 0.9792 | 0.9838 | 0.9990 | 1.9383 | 2.1696 |
| warm-60000 | str_methods | 0.6424 | 0.6454 | 0.5640 | 0.9665 | 0.9551 | 0.9990 | 1.8151 | 2.1696 |
| warm | str_methods | 0.6909 | 0.7068 | 0.5627 | 0.9610 | 0.9659 | 0.9966 | 1.9351 | 2.1654 |
| warm | list_ops | 0.9993 | 0.9953 | 1.0065 | 0.9520 | 0.9534 | 1.0027 | 13.7090 | 1.9541 |
| warm | attr_access | 1.0454 | 1.0473 | 0.9639 | 1.0147 | 1.0108 | 1.0021 | 3.1056 | 2.0104 |
| warm | call_overhead | 1.0123 | 1.0085 | 1.0036 | 0.9943 | 0.9873 | 1.0052 | 8.9269 | 2.0251 |
| warm | jitkernels | 1.0139 | 1.0143 | 1.0021 | 0.9949 | 0.9974 | 1.0005 | 0.8494 | 1.9607 |

The refreshed full census uses five alternating paired cycles for all 24 workloads. Its base is the original 9a69c41 checkpoint and its previous reference is the 8e86 GC-index release, the preceding complete census. These labels differ from the focused comparison above. Aggregates are geometric means of per-workload median paired ratios. Workload time excludes the startup fixture, leaving 23 timers; process elapsed time, CPU, and peak RSS include all 24.

| Metric | JIT/checkpoint | Interpreter/checkpoint | JIT/GC-index | Interpreter/GC-index | JIT/CPython | Interpreter/CPython |
|---|---:|---:|---:|---:|---:|---:|
| ns | 0.8331 | 0.9689 | 0.9880 | 0.9985 | 3.2510 | 8.7644 |
| wall_ns | 0.8966 | 0.9792 | 0.9879 | 1.0014 | 3.1501 | 5.4586 |
| cpu_ns | 0.8931 | 0.9786 | 0.9885 | 1.0012 | 3.2346 | 5.6731 |
| rss_bytes | 0.9469 | 0.9418 | 0.9999 | 1.0011 | 2.0960 | 1.9306 |

JIT workload-time wins against CPython: 6/23. Peak-RSS wins: 0/24. The objective of superiority across every meaningful metric remains unachieved.

| Fixture | Time/GC-index | CPU/GC-index | RSS/GC-index | Time/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|
| fannkuch | 0.9996 | 1.0004 | 1.0016 | 10.4077 | 1.9860 |
| nbody | 0.9908 | 0.9926 | 0.9989 | 8.8727 | 1.9819 |
| fib | 1.0010 | 0.9909 | 1.0011 | 2.9750 | 2.0108 |
| pidigits | 1.0083 | 1.0082 | 0.9990 | 0.8881 | 1.9979 |
| pyaes | 1.0027 | 0.9901 | 0.9979 | 0.6509 | 1.9894 |
| richards | 1.0264 | 1.0194 | 0.9995 | 8.2887 | 2.0022 |
| sumvm | 1.0056 | 0.9863 | 1.0005 | 0.0565 | 2.0152 |
| nested_loops | 0.9987 | 1.0038 | 0.9973 | 0.0808 | 2.0032 |
| jitloop | 1.0051 | 0.9961 | 1.0043 | 0.0734 | 2.0151 |
| jitkernels | 1.0090 | 0.9963 | 0.9968 | 0.8862 | 1.9882 |
| deltablue | 1.0347 | 1.0321 | 1.0000 | 20.4405 | 2.1779 |
| float_math | 1.0119 | 1.0096 | 0.9835 | 7.5781 | 2.9959 |
| spectral_norm | 0.9841 | 0.9864 | 1.0000 | 2.2323 | 2.0107 |
| json_bench | 1.0025 | 0.9968 | 0.9992 | 1.1518 | 2.5213 |
| str_methods | 0.6534 | 0.7183 | 1.0522 | 2.0211 | 2.1853 |
| dict_ops | 1.0005 | 0.9879 | 1.0016 | 5.5238 | 1.9775 |
| list_ops | 0.9850 | 0.9833 | 1.0016 | 13.7817 | 1.9764 |
| attr_access | 1.0503 | 1.0383 | 0.9601 | 3.1722 | 2.0375 |
| call_overhead | 1.0141 | 1.0151 | 1.0000 | 9.2625 | 2.0473 |
| generators | 1.0229 | 1.0222 | 1.0016 | 9.6295 | 2.0075 |
| deque_ops | 0.9951 | 0.9936 | 0.9986 | 16.7602 | 2.0262 |
| datetime_ops | 1.0025 | 1.0022 | 1.0005 | 15.9624 | 2.1459 |
| pickle_bench | 1.0011 | 1.0010 | 1.0012 | 329.6049 | 2.4752 |
| startup | 1.0015 | 1.0029 | 1.0022 | 1.4574 | 1.9935 |

Every original sample and observed regression remains available. The illustrative fannkuch fixture is not canonical fannkuch; pyaes is an XOR scrambler, not AES. Earlier censuses showed unexplained CPython datetime timing variation despite matching recorded inputs. Paired comparisons within this run, rather than changes in CPython-relative ratios between runs, establish candidate improvements.

The full suite includes its startup fixture, but dedicated import/startup variants, parallel throughput, retained-population scaling, and explicit GC-pause distributions are not repeated here. CPython is the recorded 3.14.7 GIL build; native execution remains gated in WeavePy GIL-disabled mode. No controlled build-latency, energy, or free-threaded CPython claim is made.
