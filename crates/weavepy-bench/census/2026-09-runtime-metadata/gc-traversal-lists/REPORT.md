# Collector traversal lists

The collector now traverses its existing candidate and discovered-object lists directly. It no longer copies all handles into separate discovery and marking vectors, and the generation snapshot reserves its required size once. The tables below record every measured gain and regression. The universal CPython performance objective remains unachieved.

Full-census timing remains provisional. A late-run snapshot recorded one-, five-, and fifteen-minute load averages of 12.9, 17.4, and 14.3 on an eight-core host. Both process CPU times and elapsed/CPU ratios vary across samples. For datetime, the preceding JIT workload samples range from 8.6 to 20.5 seconds and the candidate samples from 6.7 to 17.6 seconds. The observed aggregate ratios below do not establish a broad causal speedup. The apparent large datetime gain, interpreted string-method and JIT-kernel regressions, and list-operation regressions need quieter confirmation. All original samples and a separate variation record are retained.

## Implementation and validation

Discovery visits the original candidates, then the growing list of temporary candidates, in the same order as before. Only the currently scanned handle needs a clone while discovery extends the temporary list. Marking and resurrection scans clone iterator positions instead of Arc handles. The original lists and lookup map retain ownership throughout collection. Object reference-count seeds, field traversal, finalizer handling, and generation order retain their logic. No unsafe code, object layout, compiler, or JIT eligibility changes.

The new regression constructs twelve-node cycles through 96 levels of tuples, iterators, and mixed wrappers. It checks retained roots after generation 0, 1, and 2 collections, then checks that a full collection clears weak references after roots are dropped. Finalizers also resurrect objects whose deeply nested tuple chains must remain intact; subsequent collections check that resurrected roots survive and finalizers do not run twice. CPython and the preceding release pass the preflight checks. All 27 targeted candidate checks pass across JIT, interpreted, and GIL-disabled execution.

All 341 VM tests, formatting, Clippy, and the no-default-features check pass. The correct CLI release was rebuilt. All 275 compatibility checks pass in one complete outside-sandbox run with the existing conformance expectations. Unchanged JIT/compiler package tests were not repeated. These checks do not establish complete CPython compatibility.

The release SHA-256 is `b9e580991c6ee99c0ac9700634d176d129d8cacbf2eb75a455b10f1216bbf60c`. Its size is 44,289,168 bytes, 272 bytes smaller than the preceding release. All source, measurement-input, and extra-file snapshot hashes are verified. The isolated source diff is retained alongside the preceding source and the new regression test.

## Measurements

Focused cases use seven alternating measured pairs after a discarded warm cycle. Values are checked against CPython before and after warmup in JIT, interpreted, and GIL-disabled processes. Each executable has a separate frozen cache whose artifacts remain unchanged throughout timing. No samples are excluded. Ratios below one mean less time or memory. Wall time, CPU time, and peak RSS come from the OS outside the tool filesystem sandbox.

Retained construction batches hold 10,000 new objects and include preceding-batch cleanup. Cold-code controls include compilation and class creation. Explicit-collection graphs are prepared outside the workload timer with automatic GC disabled. Small graphs use 3,000 nodes where applicable and time 20 full collections. Large graphs time five full collections: list rings contain 30,000 or 100,000 nodes, a slotted ring contains 100,000 nodes each pointing through one tuple, and another contains 10,000 nodes each pointing through 16 nested tuples. Verification checks graph links before and after 60 warmup collections. These are batched costs, not individual GC-pause percentiles. RSS includes setup, warmup, and execution, so it must not be described as memory used by the collector alone.

The full-census aggregate changes from 8c8 are jit workload time: 1.51 percent less, interp workload time: 0.66 percent less, jit peak RSS: 0.20 percent less, interp peak RSS: 0.01 percent less. These are geometric means, not universal improvements.

The original controls have substantial elapsed-time variation. For example, one preceding JIT setter sample took 63.1 ms elapsed but about 6 ms CPU time. The original interpreted slot-update median ratio is 1.6873 elapsed and 1.0262 CPU. Both releases and CPython show varying elapsed/CPU ratios, so this record alone cannot assign the elapsed regression to the runtime change. The original samples remain intact. Before inspecting any follow-up results, one fresh run of all ten controls was planned with 15 pairs, together with seven pairs for two setter loops using 200,000 iterations. The latter retain the timed source but change the work size from 2,000; they answer a different duration question and do not replace the original controls. Every result appears below.

The fresh controls do not reproduce the large setter elapsed-time regression. Median workload elapsed/CPU ratios are about 1.001 to 1.002 for both releases in the setter cases. At 200,000 iterations, slot updates take 0.9992 times preceding JIT time and 0.9886 times preceding interpreted time; reinsertion takes 0.9999 and 0.9932 times, respectively. This supports treating the original large elapsed result as unresolved timing variation rather than a demonstrated code slowdown of that magnitude. Smaller regressions remain in the fresh run: long-name construction takes 1.0137 times preceding JIT time and 1.0290 times interpreted time; the assignment hook takes 1.0179 and 1.0123 times, and the interpreted property setter takes 1.0101 times. No further control reruns were used to select favorable samples.

### Batches

| Case | Mode | Work/8c8 | Work CPU/8c8 | Process/8c8 | CPU/8c8 | RSS/8c8 | Work/CPython | Work CPU/CPython | Process/CPython | CPU/CPython | RSS/CPython |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| slots_1 | jit | 1.0233 | 1.0234 | 1.0175 | 1.0177 | 0.9944 | 13.0043 | 13.0273 | 2.5473 | 2.6815 | 2.0637 |
| slots_1 | interp | 1.0153 | 1.0153 | 1.0071 | 1.0078 | 0.9929 | 12.0507 | 12.0678 | 2.3594 | 2.4750 | 1.9169 |
| slots_2 | jit | 1.0217 | 1.0216 | 1.0260 | 1.0258 | 0.9968 | 13.6860 | 13.7257 | 2.6437 | 2.7866 | 2.1224 |
| slots_2 | interp | 1.0110 | 1.0108 | 1.0065 | 1.0059 | 0.9941 | 12.7013 | 12.7468 | 2.4668 | 2.5935 | 1.9756 |
| slots_8 | jit | 0.9955 | 0.9954 | 0.9945 | 0.9941 | 0.9971 | 15.4211 | 15.4386 | 3.1394 | 3.3186 | 2.2510 |
| slots_8 | interp | 1.0120 | 1.0121 | 1.0145 | 1.0149 | 0.9950 | 15.2614 | 15.2757 | 3.0485 | 3.2212 | 2.0941 |
| slots_9 | jit | 0.9910 | 0.9908 | 1.0055 | 1.0046 | 0.9969 | 16.8746 | 16.8837 | 3.4350 | 3.6334 | 2.3856 |
| slots_9 | interp | 1.0053 | 1.0052 | 1.0037 | 1.0037 | 0.9937 | 16.6615 | 16.6713 | 3.2969 | 3.4770 | 2.2261 |
| slots_16 | jit | 1.0035 | 1.0032 | 1.0087 | 1.0082 | 0.9994 | 17.3364 | 17.3529 | 4.2019 | 4.4254 | 3.1524 |
| slots_16 | interp | 1.0067 | 1.0067 | 1.0034 | 1.0041 | 0.9951 | 16.7606 | 16.7763 | 3.9786 | 4.1918 | 2.9918 |
| dict_2 | jit | 1.0037 | 1.0038 | 1.0049 | 1.0069 | 0.9927 | 13.3615 | 13.3782 | 2.7100 | 2.8550 | 2.2025 |
| dict_2 | interp | 1.0152 | 1.0153 | 1.0032 | 1.0025 | 0.9922 | 12.2952 | 12.3121 | 2.5554 | 2.6927 | 2.0589 |
| dict_10 | jit | 1.0032 | 1.0032 | 1.0076 | 1.0062 | 0.9961 | 13.4916 | 13.4978 | 3.2692 | 3.4342 | 2.5769 |
| dict_10 | interp | 1.0007 | 1.0007 | 0.9926 | 0.9930 | 0.9930 | 13.1915 | 13.1897 | 3.1710 | 3.3323 | 2.4349 |
| retained_date | jit | 1.0138 | 1.0137 | 1.0090 | 1.0089 | 1.0009 | 33.0288 | 33.1035 | 3.7924 | 4.0597 | 2.3089 |
| retained_date | interp | 1.0085 | 1.0076 | 0.9948 | 0.9937 | 1.0019 | 33.1142 | 33.1612 | 3.7189 | 3.9789 | 2.1688 |
| retained_time | jit | 1.0086 | 1.0085 | 1.0074 | 1.0101 | 1.0004 | 38.9673 | 39.0496 | 4.5123 | 4.8207 | 2.4317 |
| retained_time | interp | 1.0100 | 1.0098 | 1.0005 | 1.0004 | 1.0026 | 39.1741 | 39.2238 | 4.4642 | 4.7669 | 2.2905 |
| retained_datetime | jit | 1.0042 | 1.0042 | 1.0161 | 1.0166 | 0.9993 | 37.3217 | 37.3754 | 5.7458 | 6.1572 | 2.8053 |
| retained_datetime | interp | 1.0097 | 1.0097 | 1.0033 | 1.0038 | 1.0015 | 37.9286 | 37.9828 | 5.7427 | 6.1332 | 2.6706 |
| retained_timedelta | jit | 1.0166 | 1.0166 | 1.0183 | 1.0158 | 0.9996 | 9.6160 | 9.6180 | 3.4641 | 3.6160 | 2.4010 |
| retained_timedelta | interp | 1.0076 | 1.0075 | 0.9919 | 0.9912 | 1.0022 | 9.5898 | 9.5894 | 3.3962 | 3.5726 | 2.2682 |

### Collections

| Case | Mode | Work/8c8 | Work CPU/8c8 | Process/8c8 | CPU/8c8 | RSS/8c8 | Work/CPython | Work CPU/CPython | Process/CPython | CPU/CPython | RSS/CPython |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| empty_graph | jit | 0.9633 | 0.9635 | 1.0144 | 1.0127 | 1.0005 | 0.9208 | 0.9208 | 1.2310 | 1.2534 | 1.9422 |
| empty_graph | interp | 0.9820 | 0.9820 | 0.9706 | 0.9735 | 0.9965 | 0.9353 | 0.9346 | 1.2162 | 1.2324 | 1.8099 |
| list_ring | jit | 0.9207 | 0.9207 | 0.9824 | 0.9900 | 0.9954 | 1.2196 | 1.2196 | 1.3192 | 1.3426 | 1.9168 |
| list_ring | interp | 0.9348 | 0.9348 | 1.0221 | 1.0134 | 1.0033 | 1.1518 | 1.1517 | 1.3369 | 1.3559 | 1.7869 |
| dict_ring | jit | 0.9545 | 0.9551 | 0.9816 | 0.9782 | 0.9944 | 1.3810 | 1.3830 | 1.5331 | 1.5399 | 1.9682 |
| dict_ring | interp | 0.9420 | 0.9402 | 0.9613 | 0.9618 | 0.9995 | 1.4039 | 1.3957 | 1.4907 | 1.5074 | 1.8312 |
| slot_tuple_graph | jit | 0.9844 | 0.9839 | 0.9473 | 0.9533 | 0.9877 | 2.5877 | 2.5836 | 1.9938 | 2.0583 | 2.0476 |
| slot_tuple_graph | interp | 0.9947 | 0.9775 | 0.9582 | 0.9662 | 0.9933 | 2.6615 | 2.6407 | 1.9740 | 2.0939 | 1.9063 |

### Large

| Case | Mode | Work/8c8 | Work CPU/8c8 | Process/8c8 | CPU/8c8 | RSS/8c8 | Work/CPython | Work CPU/CPython | Process/CPython | CPU/CPython | RSS/CPython |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| list_30000 | jit | 0.9096 | 0.9093 | 0.9858 | 0.9855 | 0.9888 | 2.3035 | 2.3035 | 2.2456 | 2.3421 | 1.8384 |
| list_30000 | interp | 0.9189 | 0.9188 | 0.9756 | 0.9760 | 0.9945 | 2.3177 | 2.3179 | 2.2092 | 2.2867 | 1.7601 |
| list_100000 | jit | 0.9617 | 0.9617 | 0.9926 | 0.9917 | 0.9980 | 3.7474 | 3.7480 | 3.4240 | 3.4978 | 1.7341 |
| list_100000 | interp | 0.8887 | 0.8887 | 0.9623 | 0.9626 | 0.9988 | 3.7585 | 3.7589 | 3.3824 | 3.4541 | 1.6810 |
| slot_tuple_100000 | jit | 0.8366 | 0.8421 | 0.8710 | 0.8760 | 0.9755 | 11.7409 | 11.7571 | 8.1412 | 8.3977 | 2.1533 |
| slot_tuple_100000 | interp | 0.8433 | 0.8835 | 0.8875 | 0.8940 | 0.9828 | 11.3720 | 11.4059 | 8.1337 | 8.3032 | 2.1227 |
| nested_tuple_10000 | jit | 1.0051 | 1.0035 | 1.0405 | 1.0429 | 0.9572 | 6.1552 | 6.1357 | 4.4045 | 4.5284 | 1.5648 |
| nested_tuple_10000 | interp | 0.8738 | 0.8758 | 0.9333 | 0.9336 | 0.9442 | 6.1372 | 6.0804 | 4.2183 | 4.3351 | 1.5227 |

### Controls

| Case | Mode | Work/8c8 | Work CPU/8c8 | Process/8c8 | CPU/8c8 | RSS/8c8 | Work/CPython | Work CPU/CPython | Process/CPython | CPU/CPython | RSS/CPython |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| cold_slots_1_1_calls | jit | 0.9931 | 0.9925 | 0.9647 | 0.9889 | 1.0011 | 1.6380 | 1.6115 | 1.6584 | 1.6122 | 3.4805 |
| cold_slots_1_1_calls | interp | 1.0156 | 1.0113 | 1.0543 | 1.0306 | 0.9974 | 1.4740 | 1.4596 | 1.4326 | 1.4421 | 1.8865 |
| cold_slots_1_2_calls | jit | 0.9612 | 0.9742 | 0.9017 | 1.0012 | 1.0017 | 1.5688 | 1.6973 | 1.6630 | 1.7586 | 3.4602 |
| cold_slots_1_2_calls | interp | 0.9783 | 0.9948 | 0.9994 | 0.9945 | 0.9974 | 1.4732 | 1.5491 | 1.4500 | 1.5301 | 1.8852 |
| cold_slots_9_1_calls | jit | 0.9973 | 1.0075 | 1.0671 | 1.0087 | 1.0026 | 1.2692 | 1.4088 | 1.3171 | 1.4722 | 4.2243 |
| cold_slots_9_1_calls | interp | 1.0552 | 0.9997 | 1.0573 | 1.0006 | 1.0000 | 1.3249 | 1.3496 | 1.1994 | 1.3391 | 2.0148 |
| cold_slots_9_2_calls | jit | 1.0212 | 0.9625 | 1.0157 | 1.0091 | 1.0000 | 1.4887 | 1.4715 | 1.4804 | 1.5004 | 4.2262 |
| cold_slots_9_2_calls | interp | 0.9861 | 0.9812 | 0.9834 | 0.9876 | 0.9995 | 1.4543 | 1.4359 | 1.3794 | 1.4045 | 2.0208 |
| slot_updates | jit | 0.9582 | 0.9956 | 0.9974 | 1.0214 | 1.0005 | 42.2252 | 24.8791 | 1.6009 | 1.5873 | 1.9669 |
| slot_updates | interp | 1.6873 | 1.0262 | 0.7665 | 0.9504 | 1.0029 | 32.0765 | 14.1729 | 1.2198 | 1.4051 | 1.7886 |
| slot_reinsertion | jit | 0.8401 | 0.9794 | 1.1677 | 0.9927 | 0.9978 | 15.4155 | 15.2304 | 1.4549 | 1.5032 | 1.9230 |
| slot_reinsertion | interp | 1.0498 | 1.0357 | 1.0312 | 0.9798 | 0.9994 | 16.5136 | 15.3561 | 1.3703 | 1.4912 | 1.7840 |
| explicit_descriptor | jit | 1.0093 | 1.0079 | 1.0582 | 1.0330 | 0.9968 | 8.1730 | 8.1803 | 1.5007 | 1.5554 | 1.9592 |
| explicit_descriptor | interp | 0.9970 | 1.0000 | 0.9648 | 0.9735 | 0.9931 | 6.2991 | 6.3132 | 1.3903 | 1.4348 | 1.8015 |
| assignment_hook | jit | 1.0160 | 1.0133 | 1.0062 | 1.0049 | 0.9984 | 12.0832 | 12.0865 | 1.7199 | 1.7705 | 1.9549 |
| assignment_hook | interp | 0.9998 | 1.0000 | 0.9960 | 0.9979 | 0.9983 | 8.6685 | 8.6772 | 1.5405 | 1.6057 | 1.8002 |
| property_setter | jit | 0.9991 | 0.9988 | 1.0052 | 1.0144 | 0.9973 | 11.8345 | 12.0143 | 1.4674 | 1.5243 | 1.9530 |
| property_setter | interp | 1.0000 | 0.9987 | 0.9912 | 0.9883 | 0.9959 | 10.7539 | 10.9444 | 1.3976 | 1.4388 | 1.7962 |
| long_slot_names | jit | 0.9777 | 0.9790 | 1.0022 | 1.0022 | 0.9984 | 17.8509 | 17.9066 | 1.6627 | 1.7383 | 1.9578 |
| long_slot_names | interp | 1.0143 | 1.0508 | 0.9779 | 0.9719 | 0.9972 | 14.3407 | 14.1531 | 1.5150 | 1.5813 | 1.8227 |

### Controls-confirmation

| Case | Mode | Work/8c8 | Work CPU/8c8 | Process/8c8 | CPU/8c8 | RSS/8c8 | Work/CPython | Work CPU/CPython | Process/CPython | CPU/CPython | RSS/CPython |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| cold_slots_1_1_calls | jit | 0.9930 | 0.9928 | 0.9968 | 0.9961 | 0.9997 | 1.4515 | 1.4519 | 1.5082 | 1.5148 | 3.4853 |
| cold_slots_1_1_calls | interp | 0.9949 | 0.9954 | 0.9940 | 0.9926 | 0.9974 | 1.4223 | 1.4227 | 1.4065 | 1.4119 | 1.8817 |
| cold_slots_1_2_calls | jit | 0.9942 | 0.9943 | 1.0016 | 1.0004 | 0.9986 | 1.4855 | 1.4844 | 1.5484 | 1.5538 | 3.4834 |
| cold_slots_1_2_calls | interp | 0.9937 | 0.9938 | 0.9953 | 0.9952 | 0.9969 | 1.4521 | 1.4513 | 1.4453 | 1.4535 | 1.8839 |
| cold_slots_9_1_calls | jit | 0.9958 | 0.9957 | 0.9959 | 0.9958 | 1.0000 | 1.3387 | 1.3381 | 1.3876 | 1.3901 | 4.2420 |
| cold_slots_9_1_calls | interp | 0.9980 | 0.9977 | 0.9927 | 0.9930 | 0.9975 | 1.3160 | 1.3155 | 1.3129 | 1.3169 | 2.0130 |
| cold_slots_9_2_calls | jit | 0.9906 | 0.9902 | 0.9979 | 0.9979 | 1.0005 | 1.3738 | 1.3730 | 1.4283 | 1.4306 | 4.2498 |
| cold_slots_9_2_calls | interp | 0.9968 | 0.9966 | 0.9980 | 0.9976 | 0.9985 | 1.3539 | 1.3538 | 1.3483 | 1.3511 | 2.0159 |
| slot_updates | jit | 0.9869 | 0.9868 | 1.0120 | 1.0120 | 0.9995 | 22.9974 | 23.5405 | 1.6254 | 1.6755 | 1.9729 |
| slot_updates | interp | 0.9943 | 0.9952 | 1.0045 | 1.0031 | 1.0012 | 13.9697 | 14.0267 | 1.4569 | 1.5023 | 1.7950 |
| slot_reinsertion | jit | 0.9913 | 0.9909 | 1.0112 | 1.0053 | 1.0011 | 15.4982 | 15.9328 | 1.5947 | 1.6480 | 1.9324 |
| slot_reinsertion | interp | 0.9872 | 0.9872 | 0.9905 | 0.9879 | 0.9977 | 15.8033 | 15.8992 | 1.5403 | 1.5901 | 1.7967 |
| explicit_descriptor | jit | 1.0082 | 1.0082 | 1.0098 | 1.0067 | 0.9989 | 8.0710 | 8.0593 | 1.5022 | 1.5466 | 1.9507 |
| explicit_descriptor | interp | 1.0088 | 1.0090 | 0.9946 | 0.9982 | 0.9954 | 6.4496 | 6.4701 | 1.3982 | 1.4349 | 1.8008 |
| assignment_hook | jit | 1.0179 | 1.0148 | 1.0155 | 1.0151 | 0.9984 | 12.0118 | 12.1368 | 1.7670 | 1.8392 | 1.9538 |
| assignment_hook | interp | 1.0123 | 1.0186 | 1.0045 | 0.9957 | 0.9977 | 8.5909 | 8.5806 | 1.5586 | 1.6152 | 1.7981 |
| property_setter | jit | 0.9943 | 0.9941 | 1.0126 | 1.0066 | 1.0005 | 11.9385 | 12.0282 | 1.4942 | 1.5435 | 1.9518 |
| property_setter | interp | 1.0101 | 1.0101 | 0.9866 | 0.9858 | 0.9977 | 11.0146 | 11.0857 | 1.4050 | 1.4446 | 1.7954 |
| long_slot_names | jit | 1.0137 | 1.0131 | 1.0148 | 1.0128 | 0.9974 | 17.1490 | 17.2381 | 1.6669 | 1.7239 | 1.9557 |
| long_slot_names | interp | 1.0290 | 1.0287 | 1.0039 | 0.9998 | 0.9994 | 13.4371 | 13.5176 | 1.5468 | 1.5968 | 1.8162 |

### Setter-long-work

| Case | Mode | Work/8c8 | Work CPU/8c8 | Process/8c8 | CPU/8c8 | RSS/8c8 | Work/CPython | Work CPU/CPython | Process/CPython | CPU/CPython | RSS/CPython |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| slot_updates | jit | 0.9992 | 0.9988 | 0.9982 | 0.9986 | 0.9968 | 23.6129 | 23.6126 | 10.9507 | 11.5106 | 1.9708 |
| slot_updates | interp | 0.9886 | 0.9886 | 0.9940 | 0.9940 | 1.0006 | 14.0923 | 14.0994 | 6.8336 | 7.1594 | 1.7962 |
| slot_reinsertion | jit | 0.9999 | 0.9993 | 0.9953 | 0.9944 | 1.0033 | 16.1611 | 16.1490 | 9.7430 | 10.1200 | 1.9270 |
| slot_reinsertion | interp | 0.9932 | 0.9934 | 0.9889 | 0.9921 | 0.9994 | 16.1466 | 16.1498 | 9.7662 | 10.1445 | 1.7923 |

## Full census

The 24-workload census has five alternating measured pairs, comparing checkpoint 9a69c41, preceding release 8c8e8b5a, this release, and CPython 3.14.7. Each binary uses its own stable frozen cache. Aggregates are geometric means. The startup placeholder is excluded from workload-time aggregation.

| Metric | JIT/checkpoint | Interpreter/checkpoint | JIT/8c8 | Interpreter/8c8 | JIT/CPython | Interpreter/CPython |
|---|---:|---:|---:|---:|---:|---:|
| ns | 0.7863 | 0.9396 | 0.9849 | 0.9934 | 3.3974 | 9.4744 |
| wall_ns | 0.8583 | 0.9502 | 0.9882 | 0.9927 | 3.1383 | 5.5606 |
| cpu_ns | 0.8578 | 0.9530 | 0.9946 | 0.9964 | 3.2220 | 5.8008 |
| rss_bytes | 0.9431 | 0.9334 | 0.9980 | 0.9999 | 2.0720 | 1.9008 |

WeavePy wins 6/23 JIT workload-time comparisons and 0/24 JIT peak-RSS comparisons. The overall objective remains unachieved.

| Case | Mode | Work/8c8 | Process/8c8 | CPU/8c8 | RSS/8c8 | Work/CPython | Process/CPython | CPU/CPython | RSS/CPython |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| fannkuch | jit | 1.0120 | 1.0086 | 1.0081 | 0.9995 | 9.1284 | 4.0961 | 4.3406 | 1.9676 |
| fannkuch | interp | 1.0101 | 1.0116 | 1.0076 | 1.0012 | 9.2213 | 4.0964 | 4.3303 | 1.8296 |
| nbody | jit | 0.9990 | 0.9976 | 0.9972 | 0.9995 | 8.8327 | 5.6033 | 5.7832 | 1.9577 |
| nbody | interp | 1.0134 | 1.0126 | 1.0125 | 1.0035 | 8.8491 | 5.5768 | 5.7868 | 1.7939 |
| fib | jit | 0.9961 | 1.0002 | 0.9997 | 0.9995 | 3.0012 | 2.0879 | 2.1591 | 1.9870 |
| fib | interp | 1.0032 | 1.0085 | 1.0088 | 1.0035 | 12.2949 | 6.0352 | 6.3427 | 1.8317 |
| pidigits | jit | 1.0093 | 1.0093 | 0.9998 | 1.0053 | 0.9004 | 0.9021 | 0.8954 | 1.9622 |
| pidigits | interp | 1.0037 | 1.0165 | 1.0091 | 1.0074 | 0.8916 | 0.8954 | 0.8976 | 1.8252 |
| pyaes | jit | 1.0279 | 1.0499 | 1.0096 | 0.9957 | 0.6988 | 1.0067 | 1.0305 | 1.9651 |
| pyaes | interp | 1.0158 | 1.0192 | 1.0259 | 1.0006 | 11.1004 | 5.6648 | 6.1929 | 1.7979 |
| richards | jit | 0.9821 | 0.9679 | 0.9684 | 1.0027 | 8.2087 | 3.6310 | 3.8622 | 1.9689 |
| richards | interp | 0.9899 | 0.9803 | 1.0010 | 1.0041 | 12.4931 | 5.0492 | 5.6142 | 1.8232 |
| sumvm | jit | 0.9980 | 1.0563 | 1.0299 | 0.9978 | 0.0528 | 0.5075 | 0.5023 | 1.9806 |
| sumvm | interp | 1.0016 | 0.9752 | 1.0003 | 1.0000 | 3.8350 | 2.9560 | 3.0544 | 1.8328 |
| nested_loops | jit | 0.8706 | 0.9441 | 0.9776 | 0.9973 | 0.0663 | 0.5449 | 0.5162 | 1.9806 |
| nested_loops | interp | 1.0124 | 1.0243 | 1.0001 | 0.9988 | 6.4974 | 4.7167 | 4.4938 | 1.8220 |
| jitloop | jit | 0.9942 | 0.9811 | 0.9826 | 0.9973 | 0.0617 | 0.3890 | 0.3780 | 1.9914 |
| jitloop | interp | 0.9207 | 0.9293 | 0.9299 | 1.0030 | 5.2128 | 4.0337 | 4.1549 | 1.8267 |
| jitkernels | jit | 0.9782 | 0.9820 | 0.9829 | 0.9973 | 0.8256 | 1.0986 | 1.0868 | 1.9608 |
| jitkernels | interp | 1.0650 | 1.0660 | 1.0593 | 0.9988 | 12.6824 | 7.9146 | 7.8857 | 1.8066 |
| deltablue | jit | 0.9336 | 0.9373 | 0.9961 | 0.9969 | 20.5632 | 13.9873 | 15.4072 | 2.1588 |
| deltablue | interp | 0.9749 | 0.9763 | 0.9971 | 0.9960 | 14.6212 | 9.7113 | 11.0728 | 1.8926 |
| float_math | jit | 0.9619 | 0.9662 | 0.9573 | 0.9938 | 5.8235 | 4.2664 | 4.4243 | 2.9692 |
| float_math | interp | 0.9407 | 1.0036 | 0.9594 | 0.9881 | 8.8739 | 6.6674 | 6.6406 | 2.9075 |
| spectral_norm | jit | 1.0044 | 0.9912 | 0.9919 | 0.9957 | 2.1978 | 1.9055 | 1.9392 | 1.9914 |
| spectral_norm | interp | 1.0202 | 1.0204 | 1.0201 | 0.9994 | 7.5177 | 5.2139 | 5.3701 | 1.8223 |
| json_bench | jit | 1.0090 | 1.0183 | 1.0056 | 0.9976 | 1.3685 | 1.9202 | 1.9437 | 2.4914 |
| json_bench | interp | 1.0376 | 0.9775 | 0.9958 | 0.9978 | 1.2050 | 1.6621 | 1.6742 | 2.3009 |
| str_methods | jit | 1.0184 | 0.9904 | 0.9946 | 1.0010 | 2.1177 | 1.8255 | 1.8519 | 2.1968 |
| str_methods | interp | 1.0988 | 1.0625 | 1.0527 | 0.9988 | 3.3766 | 2.4540 | 2.6043 | 1.8288 |
| dict_ops | jit | 1.0335 | 1.0249 | 1.0246 | 0.9962 | 5.5871 | 4.0003 | 4.1066 | 1.9489 |
| dict_ops | interp | 0.9882 | 0.9698 | 0.9745 | 1.0000 | 5.2786 | 3.8343 | 3.9764 | 1.8160 |
| list_ops | jit | 1.0669 | 1.0548 | 1.0489 | 1.0000 | 14.2103 | 8.0522 | 8.3450 | 1.9614 |
| list_ops | interp | 1.0527 | 1.0471 | 1.0517 | 0.9977 | 15.2941 | 8.4716 | 8.9131 | 1.8204 |
| attr_access | jit | 1.0009 | 0.9944 | 0.9952 | 0.9968 | 2.6352 | 2.1084 | 2.1740 | 2.0193 |
| attr_access | interp | 0.9868 | 0.9895 | 0.9895 | 0.9994 | 9.0621 | 5.9578 | 6.1608 | 1.8260 |
| call_overhead | jit | 0.9840 | 0.9842 | 0.9846 | 0.9963 | 8.8615 | 6.5013 | 6.6629 | 2.0355 |
| call_overhead | interp | 0.9979 | 0.9987 | 1.0000 | 0.9976 | 10.0915 | 7.3648 | 7.5303 | 1.8258 |
| generators | jit | 0.9381 | 0.9393 | 0.9833 | 0.9984 | 10.7971 | 6.2932 | 6.4165 | 1.9882 |
| generators | interp | 1.0364 | 1.0317 | 1.0314 | 1.0024 | 12.6111 | 7.2031 | 7.4660 | 1.8290 |
| deque_ops | jit | 1.0328 | 1.0285 | 1.0222 | 0.9950 | 16.5158 | 10.3207 | 10.7403 | 2.0027 |
| deque_ops | interp | 0.9879 | 0.9904 | 0.9969 | 1.0018 | 17.5245 | 10.4194 | 11.1510 | 1.9017 |
| datetime_ops | jit | 0.8258 | 0.8265 | 0.9058 | 0.9980 | 136.1677 | 69.6805 | 75.0401 | 2.1158 |
| datetime_ops | interp | 0.7751 | 0.7735 | 0.8371 | 1.0017 | 108.5047 | 60.0636 | 67.7560 | 1.9086 |
| pickle_bench | jit | 1.0085 | 0.9921 | 0.9914 | 0.9959 | 215.3179 | 40.9862 | 44.1038 | 2.4229 |
| pickle_bench | interp | 0.9620 | 0.9652 | 0.9716 | 0.9968 | 201.1897 | 39.1046 | 42.0123 | 2.2176 |
| startup | jit | 1.0008 | 1.0008 | 1.0224 | 0.9995 | 1.3656 | 1.3656 | 1.4140 | 1.9655 |
| startup | interp | 1.0176 | 1.0176 | 1.0068 | 0.9982 | 1.4045 | 1.4045 | 1.4081 | 1.8245 |

## Controlled startup

These nine conditions use 31 pairs, JIT disabled, isolated frozen caches, and explicitly verified matching or relocated source filenames. All artifacts remain unchanged through the measurements.

| Condition | Process/8c8 | CPU/8c8 | RSS/8c8 | Process/CPython | CPU/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|---:|
| startup_matched | 0.9973 | 0.9962 | 1.0006 | 1.3573 | 1.4079 | 1.8215 |
| startup_relocated | 1.0039 | 1.0039 | 0.9982 | 1.3677 | 1.4117 | 1.8266 |
| no_site_matched | 1.0132 | 1.0072 | 1.0051 | 0.6194 | 0.5916 | 1.5633 |
| imports_matched | 0.9979 | 0.9977 | 1.0021 | 2.7571 | 2.9289 | 2.4242 |
| imports_relocated | 0.9984 | 0.9996 | 1.0021 | 2.7628 | 2.9228 | 2.4290 |
| pickle_import_matched | 0.9989 | 0.9980 | 1.0014 | 2.1660 | 2.2853 | 2.2162 |
| pickle_import_relocated | 0.9975 | 1.0008 | 1.0005 | 2.1777 | 2.2981 | 2.2206 |
| accelerator_first_matched | 0.9992 | 0.9983 | 1.0000 | 2.1662 | 2.2815 | 2.2151 |
| accelerator_first_relocated | 1.0024 | 1.0049 | 1.0000 | 2.1674 | 2.2862 | 2.2185 |

## Limits

The measurements cover this macOS ARM64 host and the recorded executable identities. Historical cross-build comparisons that shared caches retain their previously documented recompilation limitation; this stage isolates caches. GIL-disabled correctness does not establish parallel performance. Energy, controlled build latency, parallel throughput, GC-pause distributions, and universal workload dominance were not established. All unfavorable rows and raw samples remain part of the assessment.
