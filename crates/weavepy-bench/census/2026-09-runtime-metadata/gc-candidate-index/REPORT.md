# Collector candidate index

The collector now uses the existing address-mixing hasher for its temporary candidate lookup map. Batched full collections take about 40 to 58 percent less time than the preceding ab743 build, and several retained-object construction workloads improve about 10 to 17 percent. Peak RSS is broadly unchanged. Interpreted retained datetime construction takes 9.3 percent longer in this run, and interpreted slot reinsertion takes 4.5 percent longer. Raw pairs vary; no samples are excluded. The universal CPython performance objective remains unachieved.

## Implementation and validation

The sole runtime change from ab743 is the type of collect_generation temporary by_id map, plus an explanatory comment. It reuses GcIndex, whose hasher mixes the upper half of the hash into its lower half so aligned allocation addresses do not cluster in table buckets. Existing distribution tests cover five address strides across three address regions. The temporary map serves insertion and identity lookup only; it is not iterated to determine traversal, finalization, or callback order. No object layout, JIT eligibility, unsafe code, or root-accounting logic changes.

All 341 VM tests, formatting, Clippy, and the no-default-features check pass. The correct CLI release was rebuilt. All 24 targeted checks pass across JIT, interpreted, and GIL-disabled modes. All 275 compatibility checks pass in one complete outside-sandbox run, with the existing conformance expectations. Unchanged JIT/compiler package tests were not repeated for this GC-only change. These checks do not establish complete CPython compatibility.

The release SHA-256 is `8c8e8b5aa8a57c3f769b9dfa8f84e26fa93c698d0dcfa0cce5eafd8549e4e7ce`. Its size is 44,289,440 bytes, 624 bytes smaller than the preceding build. The corrected source snapshot is authoritative. The initial snapshot preserved an extra file named environment.json, then overwrote that extra with its own metadata. Its extra-file hash is invalid, although runtime identity and source hashes remain valid. Both records are preserved. The helper now rejects duplicate and reserved extra basenames before creating output, and the negative check passed. Every hash in the fresh corrected snapshot was verified.

## CPU profile evidence

The preceding five-second warm construction profiles show unsigned-integer SipHash work beneath collector lookup and traversal. Separate samples of the candidate use the same verified inputs and warmup, with separate counter runs. Profiles are diagnostic samples, not elapsed-time benchmarks. The main thread waits for the interpreter worker and must not be counted as a CPU hotspot. See profile-comparison.json and the original sample files for counts and hashes.

An earlier allocation-metadata comparison found identical code_vm_ext and intern_name allocation counts in old and new zero-batch interpreted programs. It does not support code-metadata allocation as an explanation for the preceding interpreted RSS regression. Its instrumented byte totals must not be substituted for OS RSS.

## Focused and control measurements

Each case has seven alternating measured pairs after a discarded warm cycle. Each executable has its own frozen cache, verified unchanged throughout measurement. Values are checked against CPython before and after warmup in JIT, interpreted, and GIL-disabled processes. Retained batches hold 10,000 newly constructed objects, replacing and cleaning up the preceding batch. Cold-code controls include compilation and class creation. Explicit-collection graphs are prepared outside the workload timer, hold 3,000 nodes where applicable, and disable automatic GC; the timer covers 20 full collections. These are batched collection costs, not individual GC-pause percentiles. The empty-graph case still includes interpreter startup objects. All ratios below one mean less time or memory. Wall time, CPU time, and peak RSS come from the OS outside the tool filesystem sandbox.

For slots_9, collapsed unsigned-integer RandomState hash samples are 239 before and 13 after, out of 4019 and 4042 worker samples. Each count is a diagnostic observation from its own capture.

For dict_10, collapsed unsigned-integer RandomState hash samples are 253 before and 10 after, out of 4053 and 4068 worker samples. Each count is a diagnostic observation from its own capture.

For retained_datetime, collapsed unsigned-integer RandomState hash samples are 7 before and 5 after, out of 4052 and 4058 worker samples. Each count is a diagnostic observation from its own capture.

The full-census aggregate changes from ab743 are jit workload time: 0.09 percent less, interp workload time: 0.63 percent less, jit peak RSS: 0.03 percent more, interp peak RSS: 0.01 percent more. These are geometric means, not universal improvements.

### Batches

| Case | Mode | Work/ab743 | Process/ab743 | CPU/ab743 | RSS/ab743 | Work/CPython | Process/CPython | CPU/CPython | RSS/CPython |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| slots_1 | jit | 0.9196 | 0.9454 | 0.9435 | 1.0000 | 13.2897 | 2.6004 | 2.7344 | 2.0771 |
| slots_1 | interp | 0.8990 | 0.9139 | 0.9129 | 0.9985 | 11.2703 | 2.3188 | 2.4169 | 1.9302 |
| slots_2 | jit | 0.9701 | 0.9555 | 0.9539 | 1.0000 | 13.6420 | 2.6765 | 2.8084 | 2.1367 |
| slots_2 | interp | 0.9463 | 0.9394 | 0.9376 | 0.9966 | 12.5995 | 2.4707 | 2.5867 | 1.9854 |
| slots_8 | jit | 0.8654 | 0.9236 | 0.9230 | 1.0008 | 15.2371 | 3.0748 | 3.2216 | 2.2524 |
| slots_8 | interp | 0.8891 | 0.9192 | 0.9162 | 0.9987 | 14.5874 | 2.9607 | 3.0996 | 2.1030 |
| slots_9 | jit | 0.8407 | 0.8872 | 0.8858 | 1.0016 | 17.5559 | 3.4066 | 3.6079 | 2.3957 |
| slots_9 | interp | 0.8670 | 0.8306 | 0.8287 | 0.9975 | 17.1976 | 3.2315 | 3.4156 | 2.2411 |
| slots_16 | jit | 0.8588 | 0.8864 | 0.8872 | 1.0000 | 19.8346 | 4.2633 | 4.5134 | 3.1568 |
| slots_16 | interp | 0.8321 | 0.8842 | 0.8843 | 0.9976 | 18.5687 | 3.9866 | 4.2065 | 3.0091 |
| dict_2 | jit | 0.9563 | 0.9659 | 0.9642 | 1.0021 | 12.7614 | 2.6642 | 2.7825 | 2.2077 |
| dict_2 | interp | 0.9655 | 0.9592 | 0.9566 | 0.9973 | 12.3566 | 2.5179 | 2.6403 | 2.0724 |
| dict_10 | jit | 0.8535 | 0.8679 | 0.8679 | 1.0007 | 13.1804 | 3.1507 | 3.3309 | 2.6081 |
| dict_10 | interp | 0.8670 | 0.9012 | 0.8965 | 1.0000 | 13.2035 | 3.1019 | 3.2691 | 2.4670 |
| retained_date | jit | 1.0094 | 1.0043 | 1.0042 | 1.0022 | 33.1412 | 3.6877 | 3.9775 | 2.3142 |
| retained_date | interp | 1.0050 | 0.9923 | 0.9959 | 0.9949 | 34.1549 | 3.6716 | 3.9186 | 2.1627 |
| retained_time | jit | 1.0035 | 1.0089 | 1.0089 | 0.9983 | 40.0965 | 4.3930 | 4.6921 | 2.4355 |
| retained_time | interp | 0.9964 | 0.9988 | 0.9996 | 0.9952 | 40.2549 | 4.3217 | 4.6204 | 2.2847 |
| retained_datetime | jit | 1.0068 | 1.0036 | 1.0061 | 0.9996 | 38.5398 | 5.5334 | 5.9408 | 2.8192 |
| retained_datetime | interp | 1.0931 | 1.0537 | 1.0542 | 0.9993 | 38.9819 | 5.6540 | 5.9686 | 2.6759 |
| retained_timedelta | jit | 1.0001 | 1.0075 | 1.0002 | 1.0016 | 9.5481 | 3.3518 | 3.4759 | 2.4128 |
| retained_timedelta | interp | 1.0077 | 0.9937 | 0.9925 | 0.9961 | 9.6266 | 3.2731 | 3.4382 | 2.2657 |

### Collections

| Case | Mode | Work/ab743 | Process/ab743 | CPU/ab743 | RSS/ab743 | Work/CPython | Process/CPython | CPU/CPython | RSS/CPython |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| empty_graph | jit | 0.4429 | 0.7296 | 0.7245 | 1.0011 | 0.9580 | 1.2603 | 1.2833 | 1.9519 |
| empty_graph | interp | 0.4242 | 0.7004 | 0.6905 | 0.9994 | 0.9166 | 1.2069 | 1.2204 | 1.8103 |
| list_ring | jit | 0.4815 | 0.7179 | 0.7056 | 0.9990 | 1.2815 | 1.4324 | 1.4626 | 1.9252 |
| list_ring | interp | 0.4849 | 0.6908 | 0.6822 | 0.9989 | 1.2767 | 1.3623 | 1.3890 | 1.7878 |
| dict_ring | jit | 0.4754 | 0.6782 | 0.6735 | 1.0015 | 1.5342 | 1.5782 | 1.6036 | 1.9662 |
| dict_ring | interp | 0.4692 | 0.6595 | 0.6526 | 0.9940 | 1.4680 | 1.4978 | 1.5362 | 1.8229 |
| slot_tuple_graph | jit | 0.5968 | 0.7185 | 0.7133 | 0.9970 | 2.7177 | 2.1387 | 2.1973 | 2.0362 |
| slot_tuple_graph | interp | 0.5960 | 0.7065 | 0.7014 | 0.9963 | 2.7457 | 2.0876 | 2.1460 | 1.8946 |

### Controls

| Case | Mode | Work/ab743 | Process/ab743 | CPU/ab743 | RSS/ab743 | Work/CPython | Process/CPython | CPU/CPython | RSS/CPython |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| cold_slots_1_1_calls | jit | 0.9985 | 0.9789 | 0.9802 | 1.0003 | 1.5121 | 1.4874 | 1.4973 | 3.4883 |
| cold_slots_1_1_calls | interp | 0.9709 | 0.9912 | 0.9860 | 1.0005 | 1.4301 | 1.4897 | 1.5009 | 1.8892 |
| cold_slots_1_2_calls | jit | 0.9985 | 1.0140 | 1.0142 | 0.9994 | 1.4647 | 1.5712 | 1.5524 | 3.4951 |
| cold_slots_1_2_calls | interp | 1.0032 | 0.9793 | 0.9795 | 0.9959 | 1.4018 | 1.4381 | 1.4457 | 1.8964 |
| cold_slots_9_1_calls | jit | 1.0156 | 1.0288 | 1.0224 | 1.0009 | 1.3603 | 1.3996 | 1.4064 | 4.2532 |
| cold_slots_9_1_calls | interp | 1.0266 | 1.0294 | 1.0288 | 1.0005 | 1.2665 | 1.3146 | 1.3170 | 2.0249 |
| cold_slots_9_2_calls | jit | 0.9888 | 0.9605 | 0.9742 | 1.0000 | 1.4737 | 1.4995 | 1.5027 | 4.2460 |
| cold_slots_9_2_calls | interp | 0.9770 | 0.9850 | 0.9846 | 0.9995 | 1.3708 | 1.3733 | 1.3771 | 2.0199 |
| slot_updates | jit | 0.9422 | 0.9894 | 0.9883 | 1.0016 | 22.6377 | 1.5585 | 1.6591 | 1.9729 |
| slot_updates | interp | 0.9494 | 0.9739 | 0.9638 | 0.9977 | 13.5632 | 1.4116 | 1.4692 | 1.7952 |
| slot_reinsertion | jit | 1.0052 | 1.0072 | 1.0253 | 1.0000 | 16.4278 | 1.5197 | 1.5790 | 1.9376 |
| slot_reinsertion | interp | 1.0453 | 0.9670 | 0.9694 | 0.9988 | 16.1806 | 1.4812 | 1.5321 | 1.7973 |
| explicit_descriptor | jit | 0.9898 | 1.0005 | 1.0009 | 1.0037 | 8.0278 | 1.4719 | 1.5116 | 1.9654 |
| explicit_descriptor | interp | 0.9870 | 0.9760 | 0.9716 | 0.9977 | 6.2039 | 1.3523 | 1.3914 | 1.8019 |
| assignment_hook | jit | 1.0145 | 1.0029 | 1.0000 | 1.0027 | 12.0198 | 1.7157 | 1.7922 | 1.9624 |
| assignment_hook | interp | 0.9737 | 0.9885 | 0.9777 | 0.9959 | 8.4596 | 1.5295 | 1.5908 | 1.7985 |
| property_setter | jit | 1.0075 | 0.9953 | 1.0007 | 1.0021 | 11.3462 | 1.4251 | 1.4853 | 1.9622 |
| property_setter | interp | 0.9775 | 0.9752 | 0.9752 | 0.9988 | 10.2350 | 1.3348 | 1.3903 | 1.8004 |
| long_slot_names | jit | 0.9909 | 0.9885 | 0.9828 | 0.9984 | 16.5504 | 1.6386 | 1.6955 | 1.9620 |
| long_slot_names | interp | 0.9901 | 0.9743 | 0.9712 | 0.9950 | 13.4697 | 1.5174 | 1.5687 | 1.8160 |

## Full census

The 24-workload census has five alternating measured pairs, comparing checkpoint 9a69c41, preceding release ab743cf6, this release, and CPython 3.14.7. Each binary uses its own stable frozen cache. Aggregates are geometric means. The startup placeholder is excluded from workload-time aggregation.

| Metric | JIT/checkpoint | Interpreter/checkpoint | JIT/ab743 | Interpreter/ab743 | JIT/CPython | Interpreter/CPython |
|---|---:|---:|---:|---:|---:|---:|
| ns | 0.7924 | 0.9331 | 0.9991 | 0.9937 | 3.4145 | 9.3320 |
| wall_ns | 0.8521 | 0.9453 | 0.9936 | 0.9950 | 3.1702 | 5.5764 |
| cpu_ns | 0.8502 | 0.9435 | 0.9895 | 0.9968 | 3.2482 | 5.8149 |
| rss_bytes | 0.9436 | 0.9335 | 1.0003 | 1.0001 | 2.0779 | 1.9037 |

WeavePy wins 6/23 JIT workload-time comparisons and 0/24 JIT peak-RSS comparisons. The overall objective remains unachieved.

| Case | Mode | Work/ab743 | Process/ab743 | CPU/ab743 | RSS/ab743 | Work/CPython | Process/CPython | CPU/CPython | RSS/CPython |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| fannkuch | jit | 0.9886 | 0.9838 | 0.9867 | 0.9984 | 8.9531 | 4.1842 | 4.3874 | 1.9687 |
| fannkuch | interp | 1.0004 | 1.0006 | 1.0013 | 0.9994 | 9.0626 | 4.1418 | 4.3654 | 1.8231 |
| nbody | jit | 0.9953 | 0.9954 | 0.9954 | 1.0000 | 9.1792 | 5.6919 | 5.9442 | 1.9651 |
| nbody | interp | 1.0028 | 0.9997 | 1.0015 | 1.0029 | 8.9669 | 5.5696 | 5.8053 | 1.8034 |
| fib | jit | 1.0035 | 0.9907 | 0.9911 | 1.0011 | 2.9443 | 2.0622 | 2.1183 | 1.9978 |
| fib | interp | 0.9980 | 0.9996 | 0.9999 | 1.0000 | 12.0273 | 6.0341 | 6.3067 | 1.8406 |
| pidigits | jit | 0.9966 | 0.9963 | 0.9959 | 0.9958 | 0.8957 | 0.8980 | 0.8983 | 1.9753 |
| pidigits | interp | 0.9988 | 0.9993 | 1.0032 | 0.9994 | 0.8886 | 0.8924 | 0.8896 | 1.8261 |
| pyaes | jit | 1.0107 | 0.9770 | 0.9807 | 1.0005 | 0.6545 | 1.0341 | 1.0341 | 1.9725 |
| pyaes | interp | 0.9845 | 0.9940 | 0.9901 | 1.0000 | 11.7245 | 6.3291 | 6.6286 | 1.8036 |
| richards | jit | 0.9941 | 0.9955 | 0.9886 | 1.0011 | 8.5502 | 4.3099 | 4.5121 | 1.9871 |
| richards | interp | 1.0001 | 0.9973 | 0.9975 | 0.9994 | 12.5005 | 5.8599 | 6.1723 | 1.8357 |
| sumvm | jit | 0.9950 | 0.9936 | 0.9926 | 0.9973 | 0.0559 | 0.5137 | 0.4984 | 1.9849 |
| sumvm | interp | 0.9915 | 0.9993 | 0.9938 | 0.9988 | 3.6112 | 2.8078 | 2.8728 | 1.8312 |
| nested_loops | jit | 0.9942 | 0.9839 | 0.9813 | 1.0033 | 0.0813 | 0.5529 | 0.5388 | 1.9903 |
| nested_loops | interp | 0.9980 | 0.9972 | 0.9976 | 0.9994 | 6.3351 | 4.5719 | 4.7049 | 1.8276 |
| jitloop | jit | 1.0004 | 0.9785 | 0.9725 | 0.9989 | 0.0723 | 0.4485 | 0.4367 | 1.9849 |
| jitloop | interp | 0.9895 | 0.9896 | 0.9992 | 1.0012 | 5.5933 | 4.3583 | 4.4580 | 1.8142 |
| jitkernels | jit | 0.9905 | 0.9742 | 0.9784 | 1.0027 | 0.8638 | 1.0617 | 1.0659 | 1.9914 |
| jitkernels | interp | 0.9952 | 0.9896 | 0.9933 | 0.9965 | 11.6859 | 7.4513 | 7.7300 | 1.8235 |
| deltablue | jit | 1.0151 | 1.0146 | 1.0125 | 1.0013 | 19.9396 | 14.0236 | 14.6968 | 2.1675 |
| deltablue | interp | 0.9918 | 0.9923 | 0.9924 | 0.9995 | 14.5783 | 10.5181 | 10.9646 | 1.8845 |
| float_math | jit | 0.9205 | 0.9302 | 0.9296 | 1.0009 | 6.8206 | 5.0183 | 5.1376 | 2.9941 |
| float_math | interp | 0.9241 | 0.9284 | 0.9272 | 1.0011 | 9.5947 | 6.7917 | 7.0149 | 2.9519 |
| spectral_norm | jit | 1.0077 | 1.0035 | 1.0036 | 1.0000 | 2.1080 | 1.8121 | 1.8470 | 1.9925 |
| spectral_norm | interp | 0.9747 | 0.9772 | 0.9769 | 1.0018 | 6.8845 | 4.7943 | 4.9705 | 1.8273 |
| json_bench | jit | 1.0088 | 1.0090 | 1.0045 | 1.0045 | 1.1466 | 1.6602 | 1.6783 | 2.4769 |
| json_bench | interp | 0.9921 | 0.9765 | 0.9928 | 0.9938 | 1.1762 | 1.6133 | 1.6303 | 2.2847 |
| str_methods | jit | 0.9962 | 0.9933 | 0.9955 | 0.9995 | 2.0276 | 1.7437 | 1.7758 | 2.1764 |
| str_methods | interp | 1.0191 | 1.0162 | 1.0168 | 0.9994 | 3.0319 | 2.4007 | 2.4601 | 1.8398 |
| dict_ops | jit | 1.0014 | 0.9999 | 1.0005 | 1.0011 | 5.5242 | 3.9878 | 4.1067 | 1.9585 |
| dict_ops | interp | 0.9872 | 0.9924 | 0.9919 | 1.0035 | 5.5017 | 3.9434 | 4.0640 | 1.8110 |
| list_ops | jit | 1.0441 | 1.0489 | 0.9938 | 1.0005 | 14.1443 | 7.8724 | 8.2128 | 1.9678 |
| list_ops | interp | 0.9878 | 0.9936 | 0.9940 | 1.0024 | 13.9657 | 8.1361 | 8.4900 | 1.8249 |
| attr_access | jit | 0.9979 | 1.0031 | 0.9949 | 0.9984 | 2.6220 | 2.0713 | 2.1215 | 2.0139 |
| attr_access | interp | 0.9957 | 0.9954 | 0.9909 | 1.0024 | 9.5602 | 6.1982 | 6.3876 | 1.8180 |
| call_overhead | jit | 1.0217 | 1.0177 | 1.0182 | 1.0032 | 8.5960 | 6.2825 | 6.4685 | 2.0289 |
| call_overhead | interp | 1.0085 | 1.0061 | 1.0064 | 1.0041 | 9.8195 | 7.1225 | 7.3312 | 1.8310 |
| generators | jit | 1.0227 | 1.0169 | 1.0078 | 0.9989 | 10.0468 | 6.2646 | 6.4927 | 1.9946 |
| generators | interp | 0.9789 | 0.9830 | 0.9858 | 1.0035 | 10.8217 | 6.8003 | 7.0842 | 1.8238 |
| deque_ops | jit | 0.9973 | 0.9970 | 0.9971 | 0.9983 | 16.7047 | 11.0230 | 11.4047 | 2.0076 |
| deque_ops | interp | 1.0035 | 1.0020 | 1.0012 | 0.9986 | 16.8245 | 11.0003 | 11.3976 | 1.9041 |
| datetime_ops | jit | 0.9856 | 0.9838 | 0.9781 | 1.0005 | 127.7802 | 69.4105 | 72.7210 | 2.1293 |
| datetime_ops | interp | 1.0139 | 1.0137 | 1.0135 | 0.9955 | 122.7693 | 57.0495 | 69.1625 | 1.9070 |
| pickle_bench | jit | 0.9974 | 0.9919 | 0.9894 | 0.9996 | 212.9462 | 41.6777 | 45.0837 | 2.4342 |
| pickle_bench | interp | 1.0232 | 1.0238 | 1.0237 | 1.0000 | 193.4655 | 37.6218 | 40.6779 | 2.2246 |
| startup | jit | 0.9734 | 0.9734 | 0.9643 | 1.0011 | 1.3260 | 1.3260 | 1.4072 | 1.9718 |
| startup | interp | 1.0166 | 1.0166 | 1.0370 | 0.9994 | 1.4109 | 1.4109 | 1.4647 | 1.8308 |

## Controlled startup

These nine conditions use 31 pairs, JIT disabled, isolated frozen caches, and explicitly verified matching or relocated source filenames. All artifacts remain unchanged through the measurements.

| Condition | Process/ab743 | CPU/ab743 | RSS/ab743 | Process/CPython | CPU/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|---:|
| startup_matched | 0.9857 | 0.9751 | 1.0000 | 1.2835 | 1.3308 | 1.8245 |
| startup_relocated | 0.9829 | 0.9832 | 0.9994 | 1.3500 | 1.3903 | 1.8275 |
| no_site_matched | 0.9984 | 1.0029 | 0.9975 | 0.6248 | 0.5909 | 1.5547 |
| imports_matched | 0.9914 | 0.9911 | 0.9975 | 2.7177 | 2.8762 | 2.4209 |
| imports_relocated | 0.9911 | 0.9917 | 0.9967 | 2.7473 | 2.9072 | 2.4251 |
| pickle_import_matched | 0.9833 | 0.9815 | 0.9991 | 2.1925 | 2.3097 | 2.2162 |
| pickle_import_relocated | 0.9845 | 0.9841 | 0.9986 | 2.1988 | 2.3174 | 2.2198 |
| accelerator_first_matched | 0.9822 | 0.9822 | 0.9976 | 2.1723 | 2.2928 | 2.2151 |
| accelerator_first_relocated | 0.9847 | 0.9843 | 0.9976 | 2.1878 | 2.3049 | 2.2159 |

## Limits

The measurements cover this macOS ARM64 host and the recorded executable identities. Historical cross-build comparisons that shared caches retain their previously documented recompilation limitation; this stage isolates caches. GIL-disabled correctness does not establish parallel performance. Energy, controlled build latency, parallel throughput, GC-pause distributions, and universal workload dominance were not established. All unfavorable rows and raw samples remain part of the assessment.
