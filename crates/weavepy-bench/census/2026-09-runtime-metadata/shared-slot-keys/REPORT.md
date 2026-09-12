# Shared slot names

The runtime now shares existing string allocations when cached bytecode or JIT code populates slots. For retained batches of 10,000 objects with 8, 9, or 16 slots, JIT workload time falls about 14 to 16 percent and peak process RSS falls about 6 to 8 percent versus 8f1c86d7. Retained datetime RSS falls about 6 percent. Cold-code timings stay within about 1.2 percent of the preceding build, but the interpreted property-setter control takes 5.2 percent longer. These are measured tradeoffs; the universal CPython performance objective remains unachieved.

Across the full census, JIT workload time falls 0.3 percent geometrically, while interpreted workload time rises 0.6 percent. Aggregate peak RSS rises 0.6 percent with JIT and 0.3 percent interpreted. The full datetime workload improves 3.2 percent with JIT and 7.2 percent interpreted, but still takes about 124 times CPython workload time with JIT. Interpreted string methods regress 3.0 percent, and JIT dictionary operations regress 1.6 percent. The retained-batch memory gains do not establish a general reduction in process memory. These broader regressions remain follow-up work.

## Implementation and validation

`SlotStorage::insert_shared` clones a shared string only for a new key. Existing keys, replacement values, deletion order, storage promotion, and the ordinary borrowed-name API retain their behavior. Instance and slot storage layouts are unchanged, including the 32-bit fallback API. A typed JIT guard owns a shared name rather than a separate String. Generic JIT stores carry a code-owned name through the existing assignment-hook and descriptor dispatch; only a direct slot insertion retains it. The dynamic helper dereferences the same activation-owned code pointer previously read through ctx_name, and clones the name before arbitrary Python can run.

The initial integration test failed after the constructor compiled: changing the interpreter and typed JIT store alone did not cover DynAttrSet. The final test covers generic constructors and warmed typed stores after deletion and reordered insertion, with explicit native-entry checks. All 341 VM tests, 52 JIT package tests, Clippy, formatting, and the no-default-features check pass. The correct CLI release was rebuilt. All 18 targeted Python checks pass across JIT, interpreted, and GIL-disabled execution. All 275 compatibility checks pass in one complete outside-sandbox run, using the existing conformance expectations. This does not establish complete CPython language or standard-library equivalence.

The release SHA-256 is `ab743cf6a27a30c85d0eb924acdc9142c00661ed797c7589af7e95dc3b161958`. Its size is 44,290,064 bytes, 416 bytes larger than the preceding build. The source snapshot preserves 136 sources and 37 measurement inputs; later controls and analysis scripts are preserved separately. Initial failed tests, diagnostic sources, and both profile parser outputs remain available.

## Live allocations

Separate instrumented interpreter processes retain zero or 3,000 instances after warmup. The histories contain current live allocations, not allocation/free churn or execution timings. Machine-code correlation distinguishes owned string keys from container allocations. The preceding nine-slot batch added 27,000 key allocations and 864,000 instrumented bytes; the candidate adds zero at those owned-key sites. The preceding datetime batch added 30,000 allocations and 1,056,000 bytes; the candidate adds 450 allocations and 15,840 bytes. Thus, 98.5 percent of that datetime key-allocation increment disappears, while some generic interpreter stores still allocate keys. Instrumented byte counts must not be substituted for uninstrumented OS peak RSS.

The initial parser selected no groups because it omitted the executable and address columns preceding the symbol. Its zero result is invalid and preserved as a diagnostic. The corrected streaming parser matches all four histories, whose hashes agree with the original captures. Neither the profiles nor the runtime were rerun to fix the parser.

## Focused and control measurements

Each case has seven alternating measured pairs after a discarded warm cycle, with per-binary frozen caches verified unchanged throughout timing. Values are checked against CPython before and after warmup in separate JIT, interpreted, and GIL-disabled processes. Retained batches replace their preceding batch, so workload time includes its cleanup. Cold-code controls include code compilation and class creation. No samples are excluded. Some pairs vary substantially, especially short controls. Ratios below one mean less time or memory. Process time, CPU time, and peak RSS come from the OS outside the tool filesystem sandbox.

### Batches

| Case | Mode | Work/8f1c | Process/8f1c | CPU/8f1c | RSS/8f1c | Work/CPython | Process/CPython | CPU/CPython | RSS/CPython |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| slots_1 | jit | 1.0047 | 0.9719 | 0.9785 | 0.9953 | 13.6525 | 2.5413 | 2.6980 | 2.0700 |
| slots_1 | interp | 0.9725 | 0.9899 | 1.0030 | 0.9866 | 12.5199 | 2.4300 | 2.5414 | 1.9291 |
| slots_2 | jit | 0.9381 | 0.9788 | 0.9732 | 0.9869 | 13.3493 | 2.6299 | 2.7770 | 2.1380 |
| slots_2 | interp | 0.9461 | 0.9691 | 0.9712 | 0.9803 | 12.4649 | 2.4605 | 2.5909 | 1.9961 |
| slots_8 | jit | 0.8387 | 0.9016 | 0.9000 | 0.9419 | 17.7720 | 3.3977 | 3.6030 | 2.2493 |
| slots_8 | interp | 0.8931 | 0.9266 | 0.9235 | 0.9329 | 17.3891 | 3.2501 | 3.4271 | 2.1052 |
| slots_9 | jit | 0.8560 | 0.9059 | 0.9052 | 0.9412 | 18.9541 | 3.6678 | 3.8871 | 2.4019 |
| slots_9 | interp | 0.8907 | 0.9250 | 0.9258 | 0.9300 | 18.5457 | 3.4679 | 3.6811 | 2.2491 |
| slots_16 | jit | 0.8546 | 0.8905 | 0.8886 | 0.9195 | 20.8901 | 4.6394 | 4.9246 | 3.1652 |
| slots_16 | interp | 0.9004 | 0.9297 | 0.9294 | 0.9110 | 20.4430 | 4.4044 | 4.6737 | 3.0119 |
| dict_2 | jit | 1.0189 | 1.0058 | 1.0063 | 1.0039 | 13.9918 | 2.7243 | 2.9331 | 2.2077 |
| dict_2 | interp | 0.9958 | 0.9908 | 0.9935 | 0.9982 | 13.1802 | 2.6010 | 2.7544 | 2.0662 |
| dict_10 | jit | 0.9898 | 0.9937 | 0.9922 | 1.0021 | 14.5640 | 3.4310 | 3.6182 | 2.6026 |
| dict_10 | interp | 1.0002 | 1.0009 | 0.9988 | 0.9982 | 14.4248 | 3.3800 | 3.5618 | 2.4679 |
| retained_date | jit | 0.9821 | 0.9914 | 0.9923 | 0.9754 | 31.9870 | 3.7413 | 4.0088 | 2.3142 |
| retained_date | interp | 0.9491 | 0.9719 | 0.9704 | 0.9681 | 32.0630 | 3.6562 | 3.9330 | 2.1657 |
| retained_time | jit | 0.9442 | 0.9664 | 0.9643 | 0.9524 | 38.9797 | 4.4570 | 4.7559 | 2.4281 |
| retained_time | interp | 0.9574 | 0.9832 | 0.9792 | 0.9444 | 39.1343 | 4.3952 | 4.6753 | 2.2866 |
| retained_datetime | jit | 0.9676 | 0.9865 | 0.9819 | 0.9411 | 38.1533 | 5.5312 | 5.9005 | 2.8200 |
| retained_datetime | interp | 0.9057 | 0.9518 | 0.9656 | 0.9349 | 38.1685 | 5.3100 | 5.7158 | 2.6677 |
| retained_timedelta | jit | 0.9173 | 0.9596 | 0.9585 | 0.9548 | 9.3801 | 3.1921 | 3.3768 | 2.4095 |
| retained_timedelta | interp | 0.9191 | 0.9645 | 0.9644 | 0.9446 | 9.5720 | 3.2017 | 3.4281 | 2.2614 |

### Components

| Case | Mode | Work/8f1c | Process/8f1c | CPU/8f1c | RSS/8f1c | Work/CPython | Process/CPython | CPU/CPython | RSS/CPython |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| datetime_construct | jit | 0.9855 | 0.9876 | 0.9719 | 0.9875 | 30.0739 | 2.6486 | 2.8214 | 2.2322 |
| datetime_construct | interp | 0.9854 | 0.9879 | 0.9855 | 1.0033 | 28.9641 | 2.4991 | 2.6754 | 1.8878 |
| timedelta_construct | jit | 0.9534 | 0.9822 | 0.9847 | 0.9981 | 7.1957 | 2.0513 | 2.1551 | 2.1365 |
| timedelta_construct | interp | 0.9181 | 0.9706 | 0.9714 | 1.0050 | 7.2454 | 1.9435 | 2.0388 | 1.8803 |
| datetime_add | jit | 0.9633 | 0.9801 | 0.9797 | 0.9876 | 691.3805 | 7.5353 | 8.2605 | 2.2497 |
| datetime_add | interp | 0.9717 | 0.9764 | 0.9768 | 1.0028 | 673.4516 | 7.1208 | 7.7563 | 1.8823 |
| datetime_subtract | jit | 0.9724 | 0.9935 | 0.9936 | 0.9913 | 145.4731 | 2.8503 | 3.0308 | 2.1360 |
| datetime_subtract | interp | 0.9756 | 0.9880 | 0.9880 | 1.0011 | 132.4215 | 2.6889 | 2.8620 | 1.8874 |
| timedelta_multiply | jit | 0.9542 | 0.9943 | 0.9936 | 1.0072 | 21.4851 | 2.3005 | 2.4229 | 2.0469 |
| timedelta_multiply | interp | 0.9455 | 0.9700 | 0.9708 | 1.0011 | 21.3812 | 2.1804 | 2.3124 | 1.8830 |
| datetime_fields | jit | 0.9998 | 0.9994 | 1.0035 | 1.0091 | 29.1406 | 2.4368 | 2.5886 | 2.0752 |
| datetime_fields | interp | 1.0190 | 0.9848 | 0.9835 | 1.0050 | 21.8342 | 2.0833 | 2.1988 | 1.8871 |
| datetime_weekday | jit | 0.9822 | 0.9911 | 0.9915 | 1.0041 | 65.7435 | 2.0787 | 2.1891 | 2.0577 |
| datetime_weekday | interp | 1.0025 | 0.9944 | 0.9956 | 1.0050 | 37.2750 | 1.7745 | 1.8587 | 1.8940 |
| datetime_isoformat | jit | 0.9839 | 0.9825 | 0.9817 | 1.0056 | 64.7101 | 8.4597 | 9.1218 | 2.0584 |
| datetime_isoformat | interp | 0.9879 | 0.9749 | 0.9732 | 1.0044 | 64.1814 | 8.2033 | 8.8090 | 1.8882 |
| datetime_strftime | jit | 1.0290 | 1.0188 | 1.0202 | 1.0067 | 15.0117 | 4.5337 | 4.8275 | 2.0564 |
| datetime_strftime | interp | 1.0015 | 0.9969 | 1.0019 | 1.0011 | 13.0337 | 3.9929 | 4.2532 | 1.8847 |
| datetime_fromisoformat | jit | 0.9918 | 1.0294 | 0.9833 | 0.9907 | 260.4154 | 5.9794 | 6.6001 | 2.2226 |
| datetime_fromisoformat | interp | 1.0138 | 0.9553 | 0.9590 | 1.0039 | 236.1033 | 5.4727 | 6.0205 | 1.8841 |
| date_construct | jit | 0.9556 | 0.9995 | 0.9955 | 1.0000 | 35.0418 | 2.1395 | 2.2586 | 2.1148 |
| date_construct | interp | 0.9888 | 0.9883 | 0.9858 | 1.0056 | 33.8004 | 2.0196 | 2.1301 | 1.8841 |
| date_arithmetic | jit | 0.9530 | 0.9908 | 0.9903 | 0.9947 | 62.7902 | 3.5723 | 3.8146 | 2.1441 |
| date_arithmetic | interp | 0.9562 | 0.9702 | 0.9781 | 1.0011 | 60.2134 | 3.3868 | 3.5943 | 1.8809 |

### Controls

| Case | Mode | Work/8f1c | Process/8f1c | CPU/8f1c | RSS/8f1c | Work/CPython | Process/CPython | CPU/CPython | RSS/CPython |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| cold_slots_1_1_calls | jit | 1.0083 | 1.0050 | 1.0057 | 1.0034 | 1.4557 | 1.5341 | 1.5397 | 3.4843 |
| cold_slots_1_1_calls | interp | 1.0122 | 1.0107 | 1.0113 | 1.0005 | 1.4314 | 1.4274 | 1.4356 | 1.8864 |
| cold_slots_1_2_calls | jit | 1.0024 | 1.0022 | 1.0034 | 1.0042 | 1.5395 | 1.5825 | 1.5919 | 3.4868 |
| cold_slots_1_2_calls | interp | 0.9945 | 0.9967 | 0.9977 | 0.9995 | 1.4856 | 1.4748 | 1.4837 | 1.8866 |
| cold_slots_9_1_calls | jit | 1.0097 | 1.0040 | 1.0039 | 1.0019 | 1.3569 | 1.4118 | 1.4144 | 4.2473 |
| cold_slots_9_1_calls | interp | 1.0116 | 1.0035 | 1.0042 | 1.0010 | 1.3310 | 1.3298 | 1.3340 | 2.0149 |
| cold_slots_9_2_calls | jit | 1.0072 | 1.0020 | 1.0022 | 1.0024 | 1.3877 | 1.4451 | 1.4484 | 4.2356 |
| cold_slots_9_2_calls | interp | 0.9911 | 0.9927 | 0.9927 | 1.0000 | 1.3496 | 1.3569 | 1.3610 | 2.0139 |
| slot_updates | jit | 0.8395 | 0.9809 | 0.9844 | 1.0064 | 23.7069 | 1.6116 | 1.6768 | 1.9760 |
| slot_updates | interp | 0.9932 | 0.9946 | 0.9989 | 1.0017 | 14.2197 | 1.4490 | 1.5036 | 1.8004 |
| slot_reinsertion | jit | 0.9372 | 0.9816 | 0.9960 | 1.0071 | 15.5233 | 1.5779 | 1.6214 | 1.9394 |
| slot_reinsertion | interp | 0.9458 | 0.9846 | 0.9796 | 0.9983 | 15.8969 | 1.5227 | 1.5722 | 1.7987 |
| explicit_descriptor | jit | 0.9855 | 1.0036 | 1.0060 | 1.0086 | 7.7982 | 1.4960 | 1.5449 | 1.9632 |
| explicit_descriptor | interp | 1.0093 | 0.9933 | 0.9978 | 0.9988 | 6.1130 | 1.3926 | 1.4419 | 1.8042 |
| assignment_hook | jit | 0.9982 | 1.0029 | 1.0077 | 1.0054 | 12.2529 | 1.7494 | 1.8235 | 1.9549 |
| assignment_hook | interp | 0.9946 | 0.9888 | 0.9928 | 1.0006 | 8.4666 | 1.5798 | 1.6390 | 1.7954 |
| property_setter | jit | 0.9553 | 1.0138 | 1.0156 | 1.0108 | 12.0682 | 1.5022 | 1.5590 | 1.9622 |
| property_setter | interp | 1.0521 | 0.9881 | 0.9902 | 1.0035 | 11.0736 | 1.4054 | 1.4441 | 1.8031 |
| long_slot_names | jit | 0.9068 | 0.9862 | 0.9862 | 0.9287 | 17.5040 | 1.6891 | 1.7621 | 1.9651 |
| long_slot_names | interp | 0.9252 | 0.9719 | 0.9663 | 0.9176 | 13.9516 | 1.5546 | 1.6136 | 1.8218 |

## Full census

The 24-workload census has five alternating measured pairs, comparing checkpoint 9a69c41, preceding release 8f1c86d7, this release, and CPython 3.14.7. Each binary uses its own stable frozen cache. Aggregates are geometric means. The startup placeholder is excluded from workload-time aggregation.

| Metric | JIT/checkpoint | Interpreter/checkpoint | JIT/8f1c | Interpreter/8f1c | JIT/CPython | Interpreter/CPython |
|---|---:|---:|---:|---:|---:|---:|
| ns | 0.7917 | 0.9433 | 0.9969 | 1.0056 | 3.4204 | 9.3864 |
| wall_ns | 0.8538 | 0.9528 | 0.9965 | 1.0070 | 3.2005 | 5.6224 |
| cpu_ns | 0.8521 | 0.9515 | 0.9979 | 1.0079 | 3.2907 | 5.8307 |
| rss_bytes | 0.9427 | 0.9352 | 1.0056 | 1.0035 | 2.0795 | 1.9068 |

WeavePy wins 6/23 JIT workload-time comparisons and 0/24 JIT peak-RSS comparisons. The overall objective remains unachieved.

| Case | Mode | Work/8f1c | Process/8f1c | CPU/8f1c | RSS/8f1c | Work/CPython | Process/CPython | CPU/CPython | RSS/CPython |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| fannkuch | jit | 1.0032 | 0.9988 | 0.9989 | 1.0066 | 8.9092 | 4.0261 | 4.2929 | 1.9687 |
| fannkuch | interp | 0.9997 | 1.0010 | 1.0012 | 1.0018 | 9.0936 | 4.0516 | 4.2695 | 1.8252 |
| nbody | jit | 1.0141 | 1.0090 | 1.0094 | 1.0071 | 8.6592 | 5.3989 | 5.6195 | 1.9692 |
| nbody | interp | 1.0146 | 1.0170 | 1.0176 | 1.0059 | 8.8003 | 5.4086 | 5.6480 | 1.8063 |
| fib | jit | 0.9925 | 0.9847 | 0.9870 | 1.0077 | 3.0113 | 2.0913 | 2.1431 | 1.9967 |
| fib | interp | 1.0073 | 1.0075 | 1.0077 | 1.0012 | 12.1599 | 5.9163 | 6.2261 | 1.8435 |
| pidigits | jit | 0.9950 | 0.9944 | 0.9939 | 0.9995 | 0.8923 | 0.8966 | 0.8965 | 1.9855 |
| pidigits | interp | 0.9993 | 0.9998 | 0.9979 | 0.9955 | 0.8845 | 0.8873 | 0.8908 | 1.8308 |
| pyaes | jit | 1.0023 | 0.9963 | 0.9974 | 1.0070 | 0.6554 | 1.0425 | 1.0429 | 1.9830 |
| pyaes | interp | 1.0144 | 1.0133 | 1.0135 | 1.0029 | 11.7484 | 6.3990 | 6.6912 | 1.8083 |
| richards | jit | 0.9981 | 0.9999 | 1.0001 | 1.0060 | 8.2866 | 4.2629 | 4.4584 | 1.9817 |
| richards | interp | 1.0269 | 1.0222 | 1.0218 | 1.0053 | 12.4207 | 5.9153 | 6.2104 | 1.8308 |
| sumvm | jit | 0.9805 | 0.9914 | 0.9940 | 1.0055 | 0.0575 | 0.5310 | 0.5142 | 1.9859 |
| sumvm | interp | 1.0096 | 1.0066 | 1.0068 | 1.0029 | 3.7016 | 2.8956 | 2.9680 | 1.8362 |
| nested_loops | jit | 0.9518 | 0.9754 | 1.0036 | 1.0055 | 0.0822 | 0.5717 | 0.5573 | 1.9925 |
| nested_loops | interp | 0.9922 | 0.9972 | 0.9967 | 1.0065 | 6.6396 | 4.7092 | 4.8289 | 1.8299 |
| jitloop | jit | 1.0091 | 0.9923 | 0.9907 | 1.0060 | 0.0728 | 0.4664 | 0.4517 | 1.9946 |
| jitloop | interp | 1.0057 | 1.0060 | 1.0055 | 1.0035 | 5.6834 | 4.3738 | 4.4623 | 1.8312 |
| jitkernels | jit | 0.9968 | 1.0014 | 1.0038 | 1.0071 | 0.8892 | 1.1158 | 1.1177 | 1.9745 |
| jitkernels | interp | 1.0293 | 1.0287 | 1.0256 | 1.0018 | 11.9484 | 7.4684 | 7.7333 | 1.8111 |
| deltablue | jit | 1.0016 | 1.0007 | 1.0007 | 1.0067 | 20.2612 | 14.4091 | 14.8594 | 2.1651 |
| deltablue | interp | 1.0145 | 1.0135 | 1.0134 | 1.0056 | 14.6846 | 10.7039 | 10.9968 | 1.8929 |
| float_math | jit | 0.9883 | 0.9878 | 0.9882 | 1.0021 | 7.2665 | 5.4045 | 5.5428 | 2.9887 |
| float_math | interp | 1.0087 | 1.0101 | 1.0099 | 1.0002 | 10.1722 | 7.3556 | 7.5556 | 2.9451 |
| spectral_norm | jit | 0.9993 | 0.9965 | 0.9992 | 1.0065 | 2.1659 | 1.8775 | 1.9139 | 1.9957 |
| spectral_norm | interp | 1.0201 | 1.0203 | 1.0202 | 1.0023 | 7.3052 | 5.0492 | 5.1990 | 1.8244 |
| json_bench | jit | 1.0005 | 1.0015 | 1.0015 | 1.0041 | 1.1569 | 1.6762 | 1.6934 | 2.4834 |
| json_bench | interp | 1.0016 | 1.0123 | 1.0135 | 1.0084 | 1.1518 | 1.6104 | 1.6278 | 2.2936 |
| str_methods | jit | 1.0047 | 0.9979 | 0.9978 | 1.0080 | 2.0029 | 1.7662 | 1.8038 | 2.1836 |
| str_methods | interp | 1.0302 | 1.0278 | 1.0317 | 1.0030 | 3.0783 | 2.4403 | 2.4988 | 1.8342 |
| dict_ops | jit | 1.0156 | 1.0142 | 1.0137 | 1.0110 | 5.5350 | 3.9876 | 4.1162 | 1.9668 |
| dict_ops | interp | 1.0177 | 1.0160 | 1.0172 | 1.0071 | 5.5907 | 4.0116 | 4.1301 | 1.8257 |
| list_ops | jit | 1.0051 | 1.0024 | 1.0025 | 1.0066 | 13.5697 | 8.0000 | 8.3301 | 1.9666 |
| list_ops | interp | 1.0156 | 1.0164 | 1.0160 | 1.0065 | 13.8272 | 8.0990 | 8.4442 | 1.8335 |
| attr_access | jit | 1.0058 | 1.0000 | 0.9994 | 1.0064 | 2.7007 | 2.1952 | 2.2341 | 2.0171 |
| attr_access | interp | 1.0065 | 1.0082 | 1.0080 | 1.0036 | 9.2979 | 6.1241 | 6.3241 | 1.8221 |
| call_overhead | jit | 0.9985 | 0.9990 | 0.9988 | 1.0059 | 8.7485 | 6.4343 | 6.5856 | 2.0280 |
| call_overhead | interp | 0.9984 | 0.9960 | 0.9961 | 1.0047 | 9.9273 | 7.2378 | 7.4575 | 1.8240 |
| generators | jit | 0.9999 | 1.0005 | 1.0005 | 1.0022 | 9.7697 | 6.1000 | 6.3560 | 1.9849 |
| generators | interp | 1.0024 | 1.0016 | 1.0017 | 1.0047 | 10.6007 | 6.5925 | 6.8702 | 1.8237 |
| deque_ops | jit | 1.0034 | 1.0037 | 1.0029 | 1.0045 | 16.4754 | 10.2341 | 11.1398 | 2.0097 |
| deque_ops | interp | 0.9939 | 0.9952 | 0.9938 | 1.0025 | 17.1987 | 10.5705 | 11.2665 | 1.9088 |
| datetime_ops | jit | 0.9679 | 0.9674 | 0.9673 | 1.0051 | 123.5084 | 69.3477 | 72.5189 | 2.1350 |
| datetime_ops | interp | 0.9278 | 0.9284 | 0.9375 | 1.0056 | 118.3646 | 66.2171 | 69.4726 | 1.9166 |
| pickle_bench | jit | 0.9969 | 0.9939 | 0.9963 | 1.0016 | 214.4927 | 38.7171 | 45.4430 | 2.4452 |
| pickle_bench | interp | 0.9973 | 1.0007 | 1.0082 | 0.9978 | 186.6228 | 36.9808 | 39.8320 | 2.2291 |
| startup | jit | 1.0075 | 1.0075 | 1.0025 | 1.0055 | 1.4263 | 1.4263 | 1.4716 | 1.9697 |
| startup | interp | 1.0259 | 1.0259 | 1.0320 | 1.0035 | 1.3370 | 1.3370 | 1.4013 | 1.8371 |

## Controlled startup

These nine conditions use 31 pairs, JIT disabled, isolated frozen caches, and explicitly verified matching or relocated source filenames. All artifacts remain unchanged through the measurements.

| Condition | Process/8f1c | CPU/8f1c | RSS/8f1c | Process/CPython | CPU/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|---:|
| startup_matched | 1.0007 | 0.9975 | 1.0042 | 1.3730 | 1.4181 | 1.8284 |
| startup_relocated | 1.0044 | 1.0034 | 1.0018 | 1.3810 | 1.4223 | 1.8268 |
| no_site_matched | 1.0184 | 1.0120 | 1.0034 | 0.6236 | 0.5913 | 1.5554 |
| imports_matched | 1.0022 | 1.0029 | 1.0013 | 2.7673 | 2.9338 | 2.4239 |
| imports_relocated | 1.0053 | 1.0041 | 1.0017 | 2.7955 | 2.9492 | 2.4265 |
| pickle_import_matched | 1.0011 | 1.0019 | 1.0019 | 2.2132 | 2.3339 | 2.2219 |
| pickle_import_relocated | 1.0056 | 1.0024 | 1.0024 | 2.2215 | 2.3458 | 2.2214 |
| accelerator_first_matched | 0.9939 | 0.9936 | 1.0010 | 2.1811 | 2.3023 | 2.2214 |
| accelerator_first_relocated | 0.9962 | 0.9971 | 1.0014 | 2.1952 | 2.3216 | 2.2225 |

## Limits

The measurements cover this macOS ARM64 host and the recorded executable identities. Historical cross-build comparisons that shared caches retain their previously documented recompilation limitation; this stage isolates caches. GIL-disabled correctness does not establish parallel performance. Energy, controlled build latency, parallel throughput, GC-pause distributions, and universal workload dominance were not established. All unfavorable rows and raw samples remain part of the assessment.
