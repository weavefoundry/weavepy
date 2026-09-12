# Validate exact integer date and time fields natively

The Python datetime implementation now validates ordinary integer date and time fields through two stateless Rust helpers. Valid exact integers return a tuple immediately. Invalid ranges, Boolean values, large integers, subclasses, floats, and other objects retain the preceding Python conversion and validation path. Public construction, descriptors, slot storage, timezone checks, and subclass handling stay in the existing code.

The date helper checks the Gregorian calendar and the public year range. The time helper checks hour, minute, second, microsecond, and fold ranges. It accepts only exact integers, so the Python validator still handles fold objects and Boolean values as before. Unsupported inputs cause no native conversion callbacks. Arbitrary rebinding of private validation globals, such as _index or _days_in_month, is not universally guarded.

Against the retained parser release 21b59ded, focused datetime and date construction take about 40 percent less workload time. Time construction takes about 35 percent less, datetime addition about 20 percent less, and date arithmetic about 27 to 28 percent less. Component peak RSS falls roughly 0.5 to 1.2 percent. These are measured process peaks, not a demonstrated change to object layouts. Custom-index and Boolean controls take about 2 to 5 percent longer. Invalid-input controls retain smaller regressions. All results, including unchanged controls, appear below.

The full datetime workload takes 14.5 percent less time with JIT enabled and 15.8 percent less interpreted than 21b59ded, while its peak RSS falls about 0.6 to 0.8 percent. It still takes about 127 times CPython JIT workload time. Full-suite geometric-mean workload time falls 0.7 percent with JIT enabled and 1.1 percent interpreted; peak RSS falls about 0.6 and 0.4 percent, respectively. Unchanged nested loops retain a 1.7 percent JIT workload regression, and pidigits retains a 0.6 percent JIT RSS regression. Controlled no-site startup takes 2.9 percent longer. These observations do not establish improvement on every metric or a causal explanation for changes in unrelated code.

## Correctness and build

All 337 VM tests passed in 66.96 seconds. Formatting and Clippy passed; the CLI release built in 4 minutes 19 seconds. That build duration is an observation, not a controlled build-speed comparison. The executable is 44,289,648 bytes, 448 bytes larger than 21b59ded. Its SHA-256 is `8f1c86d7d8407d23488e14ce8a91d7f89fca206435cdbe0fd25467fa395c6565`. The release snapshot records 135 sources and 37 measurement inputs, plus the additional field scripts. No unsafe code or object-layout source changes are introduced. Compiler/JIT/workspace/no-JIT checks and object-layout measurements were not repeated.

All 275 compatibility checks passed in one complete run outside the tool filesystem sandbox. The targeted field test additionally passes in JIT, interpreted, and GIL-disabled modes. After 2,000 warm calls, its instrumentation proves that public construction reaches both native helpers and that custom-index fields fall back. These extra checks are not silently included in the 275-check count.

The 6,448-case field and constructor differential preserves every preceding outcome in all three modes. It covers calendar and range boundaries, conversion callback order, errors, subclasses, timezones, fold values, and replace methods. Each mode accepts 761 private helper cases natively, all exactly matching the preceding helper result without callbacks. Public native coverage is established separately by the targeted test, not inferred from this private-helper count.

There are 278 existing CPython differences per mode: 149 error/error, 81 success/success, 42 WeavePy-success/CPython-error, and six WeavePy-error/CPython-success cases. Those categories include type, callback, and wording differences and must not be described as only message differences. The full exact records remain available.

A 175-case low-recursion constructor comparison introduces no exact regressions and has three CPython outcome convergences, one per mode: the valid date constructor succeeds where the preceding build raises RecursionError. Twenty-seven CPython differences remain across the three modes. The separate ISO differential reruns 5,139 cases against 21b59ded and CPython with no new regressions.

## Preserved verification failures

The first constructor oracle stopped while serializing an existing WeavePy behavior: a custom equality-based fold object is accepted and retained, while CPython rejects it. The corrected driver records fold type and stable representation. The original failure and driver remain unchanged; the correction does not modify runtime sources or the binary.

The initial 19-control verifier stopped before any control timing because WeavePy preserves a Boolean fold while CPython returns an integer. The corrected probes compare fold numerically only in their out-of-band value checker. All 19 timed fixture sources are byte-identical to the original probes. Exact fold types remain in the broader oracle. The completed 12-component run was not repeated. Both failures and the corrected measurement inputs are preserved.

## Paired components and fallback controls

Each workload uses seven paired samples after a discarded cycle, with 2,000 operations, outside the tool filesystem sandbox. Values or error types are verified before and after warmup in separate processes. Each binary has a separate frozen cache; all measured cache hashes stay unchanged. Process elapsed time, CPU, and peak RSS are OS measurements. Workload CPU is recorded separately. Below one means less time or memory.

### components: ratios against 21b59ded

| Probe | Mode | Work time | Process time | Process CPU | Peak RSS | Work CPU |
|---|---|---:|---:|---:|---:|---:|
| datetime_construct | jit | 0.5968 | 0.7718 | 0.7663 | 0.9926 | 0.5968 |
| datetime_construct | interp | 0.5861 | 0.7665 | 0.7629 | 0.9945 | 0.5860 |
| timedelta_construct | jit | 1.0035 | 1.0018 | 0.9997 | 0.9927 | 1.0034 |
| timedelta_construct | interp | 0.9916 | 0.9922 | 0.9949 | 0.9928 | 0.9911 |
| datetime_add | jit | 0.7954 | 0.8298 | 0.8277 | 0.9931 | 0.7954 |
| datetime_add | interp | 0.7974 | 0.8301 | 0.8283 | 0.9956 | 0.7974 |
| datetime_subtract | jit | 0.9803 | 1.0051 | 1.0062 | 0.9928 | 0.9803 |
| datetime_subtract | interp | 0.9803 | 0.9893 | 0.9899 | 0.9895 | 0.9803 |
| timedelta_multiply | jit | 0.9865 | 1.0055 | 1.0033 | 0.9908 | 0.9867 |
| timedelta_multiply | interp | 0.9555 | 0.9828 | 0.9822 | 0.9906 | 0.9556 |
| datetime_fields | jit | 0.9897 | 0.9990 | 0.9968 | 0.9899 | 0.9897 |
| datetime_fields | interp | 0.9976 | 0.9934 | 0.9893 | 0.9945 | 0.9975 |
| datetime_weekday | jit | 1.0018 | 1.0002 | 0.9979 | 0.9919 | 1.0025 |
| datetime_weekday | interp | 0.9984 | 1.0009 | 1.0009 | 0.9934 | 0.9984 |
| datetime_isoformat | jit | 0.9961 | 0.9991 | 0.9993 | 0.9889 | 0.9961 |
| datetime_isoformat | interp | 1.0017 | 1.0017 | 1.0010 | 0.9917 | 1.0017 |
| datetime_strftime | jit | 0.9868 | 0.9905 | 0.9894 | 0.9949 | 0.9869 |
| datetime_strftime | interp | 0.9979 | 0.9883 | 0.9886 | 0.9906 | 0.9979 |
| datetime_fromisoformat | jit | 0.8412 | 0.8759 | 0.8717 | 0.9931 | 0.8412 |
| datetime_fromisoformat | interp | 0.8333 | 0.8686 | 0.8669 | 0.9934 | 0.8333 |
| date_construct | jit | 0.5977 | 0.8480 | 0.8434 | 0.9907 | 0.5976 |
| date_construct | interp | 0.5816 | 0.8406 | 0.8350 | 0.9945 | 0.5817 |
| date_arithmetic | jit | 0.7167 | 0.8171 | 0.8142 | 0.9928 | 0.7167 |
| date_arithmetic | interp | 0.7273 | 0.8179 | 0.8138 | 0.9945 | 0.7270 |

### components: ratios against CPython

| Probe | Mode | Work time | Process time | Process CPU | Peak RSS | Work CPU |
|---|---|---:|---:|---:|---:|---:|
| datetime_construct | jit | 30.7964 | 2.7491 | 2.9174 | 2.2445 | 30.8837 |
| datetime_construct | interp | 30.6091 | 2.6413 | 2.8054 | 1.8767 | 30.6898 |
| timedelta_construct | jit | 7.4567 | 2.0739 | 2.1702 | 2.1415 | 7.4673 |
| timedelta_construct | interp | 7.3842 | 2.0103 | 2.0839 | 1.8719 | 7.3969 |
| datetime_add | jit | 732.0610 | 8.5198 | 9.3209 | 2.2824 | 742.9884 |
| datetime_add | interp | 691.5252 | 7.9411 | 8.6807 | 1.8817 | 699.5000 |
| datetime_subtract | jit | 142.2640 | 2.9333 | 3.1196 | 2.1454 | 144.7805 |
| datetime_subtract | interp | 135.5133 | 2.7519 | 2.9284 | 1.8750 | 138.5119 |
| timedelta_multiply | jit | 21.7567 | 2.3346 | 2.4747 | 2.0335 | 21.7795 |
| timedelta_multiply | interp | 21.3609 | 2.2298 | 2.3591 | 1.8784 | 21.3841 |
| datetime_fields | jit | 29.1366 | 2.4523 | 2.5955 | 2.0522 | 29.1140 |
| datetime_fields | interp | 21.4317 | 2.0869 | 2.1941 | 1.8762 | 21.4412 |
| datetime_weekday | jit | 62.5641 | 2.1034 | 2.2124 | 2.0441 | 64.7121 |
| datetime_weekday | interp | 35.9246 | 1.8177 | 1.8960 | 1.8797 | 36.2319 |
| datetime_isoformat | jit | 64.7710 | 8.7049 | 9.4212 | 2.0494 | 64.8365 |
| datetime_isoformat | interp | 64.7360 | 8.5588 | 9.2669 | 1.8825 | 64.8260 |
| datetime_strftime | jit | 14.3657 | 4.5938 | 4.8651 | 2.0462 | 14.3702 |
| datetime_strftime | interp | 12.9312 | 4.1496 | 4.4093 | 1.8757 | 12.9329 |
| datetime_fromisoformat | jit | 263.9785 | 6.6965 | 7.2697 | 2.2495 | 265.5054 |
| datetime_fromisoformat | interp | 230.0369 | 6.0612 | 6.5481 | 1.8727 | 231.4570 |
| date_construct | jit | 36.1370 | 2.1774 | 2.2996 | 2.1032 | 36.5714 |
| date_construct | interp | 34.9843 | 2.0881 | 2.1984 | 1.8745 | 35.2759 |
| date_arithmetic | jit | 62.3919 | 3.6887 | 3.9607 | 2.1505 | 62.6879 |
| date_arithmetic | interp | 60.3899 | 3.5213 | 3.7824 | 1.8776 | 60.6180 |

### controls-numeric: ratios against 21b59ded

| Probe | Mode | Work time | Process time | Process CPU | Peak RSS | Work CPU |
|---|---|---:|---:|---:|---:|---:|
| time_construct | jit | 0.6530 | 0.8468 | 0.8405 | 0.9928 | 0.6529 |
| time_construct | interp | 0.6427 | 0.8368 | 0.8339 | 0.9934 | 0.6427 |
| time_fold | jit | 0.6667 | 0.8533 | 0.8477 | 0.9913 | 0.6666 |
| time_fold | interp | 0.6655 | 0.8400 | 0.8374 | 0.9912 | 0.6656 |
| date_leap | jit | 0.5896 | 0.8273 | 0.8225 | 0.9878 | 0.5894 |
| date_leap | interp | 0.4969 | 0.7839 | 0.7785 | 0.9945 | 0.4968 |
| date_century | jit | 0.5773 | 0.8236 | 0.8163 | 0.9907 | 0.5771 |
| date_century | interp | 0.4873 | 0.7736 | 0.7673 | 0.9939 | 0.4874 |
| date_index | jit | 1.0277 | 1.0087 | 1.0082 | 0.9936 | 1.0276 |
| date_index | interp | 1.0216 | 1.0082 | 1.0076 | 0.9939 | 1.0217 |
| time_index | jit | 1.0342 | 1.0117 | 1.0102 | 0.9913 | 1.0343 |
| time_index | interp | 1.0422 | 1.0055 | 1.0085 | 0.9967 | 1.0422 |
| date_int_subclass | jit | 1.0089 | 1.0093 | 1.0073 | 0.9934 | 1.0089 |
| date_int_subclass | interp | 1.0156 | 0.9971 | 0.9982 | 0.9934 | 1.0156 |
| date_bool | jit | 1.0388 | 1.0169 | 1.0215 | 0.9936 | 1.0388 |
| date_bool | interp | 1.0315 | 1.0080 | 1.0079 | 0.9928 | 1.0305 |
| time_bool_fold | jit | 1.0475 | 1.0155 | 1.0157 | 0.9928 | 1.0474 |
| time_bool_fold | interp | 1.0278 | 1.0110 | 1.0074 | 0.9928 | 1.0278 |
| date_subclass | jit | 0.5939 | 0.8323 | 0.8296 | 0.9888 | 0.5936 |
| date_subclass | interp | 0.5067 | 0.7803 | 0.7737 | 0.9928 | 0.5068 |
| time_subclass | jit | 0.6622 | 0.8582 | 0.8505 | 0.9937 | 0.6622 |
| time_subclass | interp | 0.6515 | 0.8417 | 0.8381 | 0.9906 | 0.6515 |
| datetime_subclass | jit | 0.5616 | 0.7472 | 0.7405 | 0.9917 | 0.5616 |
| datetime_subclass | interp | 0.5148 | 0.7110 | 0.7037 | 0.9934 | 0.5148 |
| date_invalid | jit | 1.0168 | 1.0083 | 1.0112 | 0.9919 | 1.0168 |
| date_invalid | interp | 1.0161 | 1.0095 | 1.0097 | 0.9934 | 1.0161 |
| time_invalid | jit | 1.0104 | 1.0114 | 1.0107 | 0.9918 | 1.0104 |
| time_invalid | interp | 1.0146 | 1.0103 | 1.0095 | 0.9923 | 1.0146 |
| date_float | jit | 1.0119 | 1.0127 | 1.0142 | 0.9924 | 1.0119 |
| date_float | interp | 0.9961 | 0.9997 | 1.0000 | 0.9923 | 0.9960 |
| time_float | jit | 1.0107 | 1.0083 | 1.0083 | 0.9919 | 1.0107 |
| time_float | interp | 1.0146 | 1.0080 | 1.0076 | 0.9906 | 1.0138 |
| date_replace | jit | 0.6358 | 0.8264 | 0.8221 | 0.9883 | 0.6356 |
| date_replace | interp | 0.5518 | 0.7928 | 0.7884 | 0.9945 | 0.5519 |
| time_replace | jit | 0.7680 | 0.8670 | 0.8659 | 0.9938 | 0.7679 |
| time_replace | interp | 0.7380 | 0.8575 | 0.8537 | 0.9950 | 0.7380 |
| datetime_replace | jit | 0.6999 | 0.8002 | 0.7967 | 0.9931 | 0.6998 |
| datetime_replace | interp | 0.6616 | 0.7785 | 0.7743 | 0.9928 | 0.6616 |

### controls-numeric: ratios against CPython

| Probe | Mode | Work time | Process time | Process CPU | Peak RSS | Work CPU |
|---|---|---:|---:|---:|---:|---:|
| time_construct | jit | 43.3841 | 2.4071 | 2.5342 | 2.1432 | 43.5739 |
| time_construct | interp | 40.5888 | 2.2821 | 2.4042 | 1.8724 | 40.9716 |
| time_fold | jit | 23.1713 | 2.3635 | 2.5019 | 2.1503 | 23.2418 |
| time_fold | interp | 22.4527 | 2.2638 | 2.3953 | 1.8768 | 22.5224 |
| date_leap | jit | 38.2187 | 2.2160 | 2.3401 | 2.1096 | 38.6966 |
| date_leap | interp | 34.8221 | 2.0819 | 2.1969 | 1.8790 | 34.9932 |
| date_century | jit | 38.2882 | 2.1964 | 2.3108 | 2.1161 | 38.3197 |
| date_century | interp | 35.1629 | 2.0750 | 2.1900 | 1.8810 | 35.2617 |
| date_index | jit | 66.4891 | 2.9821 | 3.1880 | 2.1052 | 66.8861 |
| date_index | interp | 61.4541 | 2.8106 | 2.9872 | 1.8702 | 61.8267 |
| time_index | jit | 66.3021 | 3.2604 | 3.4935 | 2.1437 | 66.5647 |
| time_index | interp | 64.9692 | 3.0678 | 3.3008 | 1.8742 | 64.9739 |
| date_int_subclass | jit | 70.4321 | 3.4549 | 3.6946 | 2.1840 | 70.7954 |
| date_int_subclass | interp | 61.4434 | 3.1137 | 3.3400 | 1.8749 | 61.6335 |
| date_bool | jit | 78.5403 | 2.7935 | 2.9648 | 2.1086 | 78.3819 |
| date_bool | interp | 74.5785 | 2.6358 | 2.7979 | 1.8830 | 74.4375 |
| time_bool_fold | jit | 36.0988 | 2.8224 | 3.0019 | 2.1502 | 36.1433 |
| time_bool_fold | interp | 35.5716 | 2.6863 | 2.8592 | 1.8780 | 35.6928 |
| date_subclass | jit | 32.8491 | 2.1952 | 2.3260 | 2.1105 | 33.2047 |
| date_subclass | interp | 31.4016 | 2.0938 | 2.2056 | 1.8780 | 31.3486 |
| time_subclass | jit | 36.1929 | 2.3669 | 2.4954 | 2.1490 | 36.4657 |
| time_subclass | interp | 36.4022 | 2.2871 | 2.4046 | 1.8740 | 36.4631 |
| datetime_subclass | jit | 38.5494 | 2.6793 | 2.8222 | 2.2495 | 38.8075 |
| datetime_subclass | interp | 38.2272 | 2.5917 | 2.7303 | 1.8781 | 38.4195 |
| date_invalid | jit | 32.8054 | 3.9790 | 4.2047 | 2.0365 | 32.8406 |
| date_invalid | interp | 32.8623 | 3.9246 | 4.1538 | 1.8843 | 32.8991 |
| time_invalid | jit | 45.2089 | 3.8960 | 4.1752 | 2.0313 | 45.2611 |
| time_invalid | interp | 42.7047 | 3.7324 | 3.9940 | 1.8771 | 42.7505 |
| date_float | jit | 54.2317 | 4.1122 | 4.4108 | 2.0324 | 54.4279 |
| date_float | interp | 52.0370 | 3.8794 | 4.1868 | 1.8779 | 52.0983 |
| time_float | jit | 54.4922 | 4.1260 | 4.4710 | 2.0260 | 54.6903 |
| time_float | interp | 52.6552 | 3.9799 | 4.3072 | 1.8753 | 52.7137 |
| date_replace | jit | 78.2278 | 2.4688 | 2.6143 | 2.1105 | 78.9798 |
| date_replace | interp | 65.8245 | 2.2545 | 2.3801 | 1.8762 | 65.9592 |
| time_replace | jit | 123.6842 | 3.2150 | 3.4429 | 2.1590 | 124.9835 |
| time_replace | interp | 105.3443 | 2.8763 | 3.0871 | 1.8869 | 106.7500 |
| datetime_replace | jit | 172.9324 | 3.8001 | 4.0843 | 2.2586 | 174.0672 |
| datetime_replace | interp | 150.0496 | 3.3743 | 3.6397 | 1.8771 | 151.9487 |

## Full census

Five paired samples compare checkpoint 9a69c41, preceding parser release 21b59ded, and the recorded CPython 3.14.7 GIL build. All 24 frozen-cache stability guards pass. Geometric means use 24 workloads for process metrics and exclude startup only from the 23-workload timer aggregate.

| Metric | JIT/checkpoint | Interpreter/checkpoint | JIT/21b5 | Interpreter/21b5 | JIT/CPython | Interpreter/CPython |
|---|---:|---:|---:|---:|---:|---:|
| Workload time, 23 workloads | 0.7955 | 0.9338 | 0.9929 | 0.9891 | 3.4045 | 9.2682 |
| Process time, 24 workloads | 0.8499 | 0.9421 | 0.9908 | 0.9908 | 3.2223 | 5.6989 |
| CPU time, 24 workloads | 0.8484 | 0.9419 | 0.9910 | 0.9901 | 3.2952 | 5.8919 |
| Peak RSS, 24 workloads | 0.9366 | 0.9317 | 0.9943 | 0.9962 | 2.0607 | 1.8974 |

WeavePy wins 6/23 JIT workload timers and 0/24 JIT peak-RSS comparisons. The overall objective remains unachieved.

| Workload | JIT time/21b5 | Interpreter time/21b5 | JIT RSS/21b5 | Interpreter RSS/21b5 | JIT time/CPython | JIT RSS/CPython |
|---|---:|---:|---:|---:|---:|---:|
| fannkuch | 1.0012 | 1.0020 | 0.9929 | 0.9929 | 8.9655 | 1.9547 |
| nbody | 0.9922 | 0.9903 | 0.9957 | 0.9930 | 8.7393 | 1.9481 |
| fib | 0.9974 | 0.9973 | 0.9940 | 0.9953 | 2.8782 | 1.9783 |
| pidigits | 1.0074 | 1.0054 | 1.0063 | 1.0028 | 0.8956 | 1.9660 |
| pyaes | 0.9831 | 1.0055 | 0.9919 | 0.9953 | 0.6462 | 1.9617 |
| richards | 0.9873 | 0.9990 | 0.9929 | 0.9965 | 7.8837 | 1.9613 |
| sumvm | 1.0044 | 0.9898 | 0.9886 | 0.9953 | 0.0566 | 1.9687 |
| nested_loops | 1.0173 | 0.9883 | 0.9940 | 1.0006 | 0.0826 | 1.9816 |
| jitloop | 1.0034 | 0.9724 | 0.9967 | 0.9947 | 0.0739 | 1.9795 |
| jitkernels | 0.9980 | 0.9898 | 0.9930 | 0.9936 | 0.8772 | 1.9419 |
| deltablue | 1.0005 | 1.0032 | 0.9929 | 0.9934 | 19.8586 | 2.1547 |
| float_math | 1.0130 | 1.0048 | 0.9973 | 0.9997 | 7.5055 | 2.9823 |
| spectral_norm | 0.9903 | 0.9925 | 0.9925 | 0.9965 | 2.1435 | 1.9755 |
| json_bench | 1.0061 | 1.0026 | 0.9963 | 0.9938 | 1.1418 | 2.4712 |
| str_methods | 1.0092 | 0.9967 | 0.9940 | 0.9976 | 2.0153 | 2.1529 |
| dict_ops | 0.9931 | 1.0024 | 0.9913 | 0.9953 | 5.5061 | 1.9339 |
| list_ops | 1.0018 | 0.9933 | 0.9907 | 0.9988 | 13.5014 | 1.9441 |
| attr_access | 0.9807 | 0.9928 | 0.9925 | 0.9959 | 2.6933 | 2.0032 |
| call_overhead | 1.0016 | 1.0051 | 0.9926 | 0.9930 | 8.8823 | 2.0118 |
| generators | 1.0019 | 0.9874 | 0.9946 | 0.9965 | 9.6837 | 1.9774 |
| deque_ops | 1.0061 | 1.0025 | 0.9955 | 0.9953 | 16.9019 | 1.9952 |
| datetime_ops | 0.8549 | 0.8425 | 0.9925 | 0.9944 | 127.1623 | 2.1046 |
| pickle_bench | 0.9973 | 0.9963 | 1.0012 | 1.0009 | 203.2699 | 2.4309 |
| startup | 1.0035 | 1.0012 | 0.9934 | 0.9970 | 1.4033 | 1.9491 |

## Controlled startup

Thirty-one paired samples use JIT-disabled execution, equal-length executable paths, separate caches, and verified matching or relocated serialized code filenames. Frozen artifacts stay unchanged.

| Case | Elapsed/21b5 | CPU/21b5 | RSS/21b5 | Elapsed/CPython | CPU/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|---:|
| startup_matched | 0.9936 | 0.9989 | 0.9958 | 1.3726 | 1.4280 | 1.8224 |
| startup_relocated | 0.9976 | 0.9958 | 0.9941 | 1.3835 | 1.4324 | 1.8242 |
| no_site_matched | 1.0292 | 1.0234 | 0.9966 | 0.6184 | 0.5894 | 1.5494 |
| imports_matched | 0.9942 | 0.9957 | 0.9962 | 2.7909 | 2.9782 | 2.4231 |
| imports_relocated | 0.9992 | 0.9999 | 0.9958 | 2.8293 | 2.9967 | 2.4241 |
| pickle_import_matched | 0.9867 | 0.9882 | 0.9953 | 2.2206 | 2.3596 | 2.2164 |
| pickle_import_relocated | 1.0012 | 1.0009 | 0.9962 | 2.2314 | 2.3710 | 2.2214 |
| accelerator_first_matched | 0.9947 | 0.9943 | 0.9962 | 2.2043 | 2.3436 | 2.2166 |
| accelerator_first_relocated | 0.9966 | 0.9958 | 0.9958 | 2.2351 | 2.3665 | 2.2171 |

## Limits

Earlier comparisons without per-binary cache isolation retain the recompilation limitation documented in the parser stage. This stage uses isolated caches for all timings. Energy, controlled build latency, parallel throughput, GC-pause distributions, and every Python workload were not measured. GIL-disabled correctness does not establish free-threaded performance. Component gains do not establish universal CPython superiority.
