# Encode supported pickle data natively

Module-level pickle.dumps now serializes exact built-in scalars, UTF-8 strings, bytes, lists, tuples, and dictionaries in Rust for protocols 4 and 5. It writes frames directly into the final byte vector, preserving memo aliases, dictionary order, integer representation, and float bits. Strong memo pins prevent address reuse during concurrent container removal. Active-container and depth checks reject cycles and deep graphs.

The native encoder invokes no Python callbacks. Unsupported values, subclasses, surrogate strings, legacy protocols, configured callbacks, and insufficient recursion headroom return to the existing pickler. Partial native output stays private and is discarded. Weak guards check writer classes, inherited methods, dispatch functions, code/default overrides, and the dump descriptor. Tested persistent-ID and reducer changes execute through the existing path exactly once. Arbitrary private module-global rebinding is not universally guarded.

Importing _pickle before pickle now publishes the six completed accelerator exports back onto pickle, matching the opposite import order. The previous release left the public module with pure Python exports in that order. Both import orders are verified against CPython and in all three candidate execution modes.

All 38 compiler tests, 332 VM tests, 52 JIT tests, and static/workspace/no-JIT checks pass. The 275-check compatibility run initially passed 273 checks; two loopback socket fixtures failed with the tool sandbox's PermissionError. Both exact fixtures passed outside that sandbox on the same executable. The archive retains the unchanged initial run, exact retry inputs/results, and an explicitly combined validation file. It does not represent that combination as a second complete suite run.

The encoder differential covers 1,266 cases, accepting 642 natively in each of JIT-enabled, interpreted, and GIL-disabled modes. Every output is independently decoded by CPython. There are no new regressions or CPython value differences. Native bytes match the preceding WeavePy encoder. The 240-case recursion differential has no regressions or improvements; 82 existing CPython outcome differences per mode remain. The 1,546-stream decoder differential still accepts 282 streams per mode with no regressions and retains 58 existing malformed-input error-wording differences per mode.

The executable SHA-256 is `f46304310b505814c85e6258b59e10274975ccb12108e339b9df4f9789f987a9` and its size is 44,289,056 bytes, 39,040 bytes above the preceding decoder release (247f3eb9). The snapshot captures 132 sources and 37 measurement inputs plus supplemental scripts. All 12 measured object layouts are unchanged. No new unsafe code is introduced by this stage.

All timings use the recorded CPython 3.14.7 GIL build outside the tool filesystem sandbox. Variant order alternates by paired cycle, after one discarded preparation cycle. Ratios below one mean less time or memory. Process elapsed time, CPU, and peak RSS are actual OS measurements. Workload CPU is measured separately inside the timed process.

## Focused components and larger graphs

Seven paired samples cover the four original payload/record encoding and decoding components and eight larger encoding/decoding probes. Larger inputs contain 200,000 integers in a list or tuple, 10,000 nested dictionary/list rows, or 8 MiB of bytes. The encoding graph is built before timing. Decode probes retain the input stream without an extra unused source graph. Values, class identities, and actual native acceptance or fallback are checked before and after workload-sized warmup in separate processes.

Ratios against the preceding decoder release:

| Probe | Mode | Work time | Process time | Process CPU | Peak RSS | Work CPU |
|---|---|---:|---:|---:|---:|---:|
| payload_dumps | jit | 0.0043 | 0.1134 | 0.1089 | 0.9768 | 0.0041 |
| payload_dumps | interp | 0.0044 | 0.1121 | 0.1079 | 0.9487 | 0.0044 |
| payload_loads | jit | 0.9072 | 0.8172 | 0.7956 | 0.9843 | 0.9074 |
| payload_loads | interp | 0.9489 | 0.7902 | 0.7873 | 0.9610 | 0.9493 |
| records_dumps | jit | 0.9799 | 0.9949 | 0.9751 | 1.0046 | 0.9835 |
| records_dumps | interp | 0.9351 | 0.9676 | 0.9843 | 0.9692 | 0.9470 |
| records_loads | jit | 0.9951 | 1.0450 | 1.0429 | 1.0104 | 0.9936 |
| records_loads | interp | 1.1253 | 1.0965 | 1.0970 | 0.9724 | 1.1216 |
| large_int_list_loads | jit | 1.0148 | 1.0358 | 1.0340 | 1.0004 | 1.0139 |
| large_int_list_loads | interp | 0.9546 | 0.9771 | 0.9805 | 0.9716 | 0.9555 |
| large_int_list_dumps | jit | 0.0017 | 0.0170 | 0.0166 | 1.1043 | 0.0016 |
| large_int_list_dumps | interp | 0.0017 | 0.0181 | 0.0174 | 1.0906 | 0.0017 |
| large_int_tuple_loads | jit | 0.9590 | 1.0345 | 1.0441 | 1.0020 | 0.9584 |
| large_int_tuple_loads | interp | 0.9527 | 0.9334 | 0.9486 | 0.9759 | 0.9530 |
| large_int_tuple_dumps | jit | 0.0014 | 0.0227 | 0.0214 | 1.0003 | 0.0014 |
| large_int_tuple_dumps | interp | 0.0014 | 0.0173 | 0.0166 | 0.9843 | 0.0014 |
| large_dicts_loads | jit | 0.9948 | 1.0019 | 1.0000 | 1.0030 | 1.0003 |
| large_dicts_loads | interp | 1.1006 | 1.0141 | 1.0372 | 0.9894 | 1.0990 |
| large_dicts_dumps | jit | 0.0051 | 0.0380 | 0.0322 | 0.8902 | 0.0050 |
| large_dicts_dumps | interp | 0.0053 | 0.0358 | 0.0353 | 0.8690 | 0.0053 |
| large_bytes_loads | jit | 1.0706 | 0.9739 | 1.0076 | 0.9997 | 1.0698 |
| large_bytes_loads | interp | 1.0520 | 0.9262 | 0.9403 | 0.9769 | 1.0501 |
| large_bytes_dumps | jit | 0.9974 | 0.9686 | 1.0109 | 0.9996 | 0.9958 |
| large_bytes_dumps | interp | 0.9646 | 0.9447 | 0.9460 | 0.9825 | 0.9621 |

Ratios against CPython:

| Probe | Mode | Work time | Process time | Process CPU | Peak RSS | Work CPU |
|---|---|---:|---:|---:|---:|---:|
| payload_dumps | jit | 3.3087 | 2.1577 | 2.3789 | 2.3283 | 3.3169 |
| payload_dumps | interp | 3.2675 | 2.0377 | 2.1677 | 2.1526 | 3.2760 |
| payload_loads | jit | 4.4252 | 2.2112 | 2.3031 | 2.3065 | 4.4291 |
| payload_loads | interp | 4.6277 | 2.0972 | 2.1929 | 2.1379 | 4.5881 |
| records_dumps | jit | 313.1933 | 27.7091 | 30.0244 | 2.4106 | 312.1992 |
| records_dumps | interp | 295.0196 | 25.2498 | 28.3176 | 2.2088 | 292.5361 |
| records_loads | jit | 314.3264 | 15.7553 | 16.9435 | 2.4151 | 312.4145 |
| records_loads | interp | 280.1630 | 14.9535 | 16.0762 | 2.2044 | 277.4465 |
| large_int_list_loads | jit | 0.4721 | 1.8389 | 1.8953 | 1.8077 | 0.4720 |
| large_int_list_loads | interp | 0.4852 | 1.6964 | 1.7513 | 1.7074 | 0.4848 |
| large_int_list_dumps | jit | 1.7925 | 1.8847 | 1.9132 | 1.9387 | 1.7977 |
| large_int_list_dumps | interp | 1.7497 | 1.6915 | 1.7901 | 1.8458 | 1.7499 |
| large_int_tuple_loads | jit | 0.8225 | 1.9517 | 1.9944 | 2.1154 | 0.8218 |
| large_int_tuple_loads | interp | 0.8194 | 1.7960 | 1.8633 | 2.0200 | 0.8186 |
| large_int_tuple_dumps | jit | 1.8047 | 2.3054 | 2.2845 | 2.1490 | 1.7743 |
| large_int_tuple_dumps | interp | 1.7600 | 1.7495 | 1.8059 | 2.0540 | 1.7773 |
| large_dicts_loads | jit | 3.1741 | 2.3030 | 2.4514 | 2.2228 | 3.0009 |
| large_dicts_loads | interp | 3.2753 | 2.3109 | 2.4170 | 2.0833 | 3.3160 |
| large_dicts_dumps | jit | 2.8521 | 2.2385 | 2.6413 | 2.1617 | 2.8417 |
| large_dicts_dumps | interp | 2.9134 | 2.0864 | 2.1862 | 2.0192 | 2.8789 |
| large_bytes_loads | jit | 0.9687 | 2.0537 | 2.1687 | 1.7128 | 0.9284 |
| large_bytes_loads | interp | 0.9517 | 1.9670 | 2.0648 | 1.6325 | 0.9523 |
| large_bytes_dumps | jit | 1.1772 | 2.0820 | 2.1836 | 1.4704 | 1.1785 |
| large_bytes_dumps | interp | 1.0739 | 1.9702 | 2.0587 | 1.4193 | 1.0762 |

The ordinary payload encodes about 231 times faster than the preceding release, but still takes about 3.3 times CPython workload time. The large integer tuple improves over 700 times, but still takes about 1.8 times CPython workload time. The large list uses about 10.4 percent more peak RSS than the preceding release because the encoder copies its contents before traversal. The nested-dictionary case uses about 11 percent less peak RSS. Large byte encoding is nearly unchanged. Custom record serialization and reconstruction remain over 300 times CPython workload time.

Some decoding timers improve without a decoder implementation change. Others regress: interpreted nested dictionaries take about 10 percent longer, and byte decoding takes about 5 to 7 percent longer. All movements remain recorded without attributing them entirely to the encoder or to noise. Isolated integer-list decoding can beat CPython while whole-process time and memory remain worse.

## Small calls and configured fallbacks

Ten controls explicitly pass protocol, fix_imports, and buffer_callback keywords. The none_default label means explicit protocol=None. Two additional probes measure ordinary pickle.dumps(value) calls without those keywords. Six decoder controls cover small values, legacy protocols, configured encoding, and buffers. Each uses seven paired samples. The encoder controls verify native eligibility, zero speculative callbacks, exact public callback counts, and round-trip values.

Encoding with explicit options, ratios against the preceding release:

| Probe | Mode | Work time | Process time | Process CPU | Peak RSS | Work CPU |
|---|---|---:|---:|---:|---:|---:|
| none_p0 | jit | 1.0491 | 1.0419 | 1.0431 | 1.0017 | 1.0484 |
| none_p0 | interp | 1.0481 | 1.0195 | 1.0166 | 0.9661 | 1.0489 |
| none_p5 | jit | 0.1387 | 0.3657 | 0.3577 | 0.9809 | 0.1383 |
| none_p5 | interp | 0.1373 | 0.3729 | 0.3649 | 0.9560 | 0.1372 |
| none_default | jit | 0.1251 | 0.3782 | 0.3684 | 0.9758 | 0.1254 |
| none_default | interp | 0.1283 | 0.3558 | 0.3480 | 0.9547 | 0.1282 |
| list_p2 | jit | 1.0397 | 1.0364 | 1.0473 | 1.0034 | 1.0421 |
| list_p2 | interp | 0.9392 | 0.9430 | 0.9411 | 0.9629 | 0.9372 |
| list_p5 | jit | 0.0633 | 0.1926 | 0.1879 | 0.9830 | 0.0632 |
| list_p5 | interp | 0.0636 | 0.1864 | 0.1822 | 0.9523 | 0.0633 |
| integer_fix_imports | jit | 1.0402 | 1.0408 | 1.0568 | 1.0055 | 1.0410 |
| integer_fix_imports | interp | 1.0712 | 1.0260 | 1.0255 | 0.9682 | 1.0677 |
| custom_fix_imports | jit | 1.0539 | 1.0646 | 1.0646 | 1.0055 | 1.0520 |
| custom_fix_imports | interp | 1.1099 | 1.1013 | 1.1013 | 0.9660 | 1.1089 |
| buffer_callback | jit | 1.0065 | 0.9819 | 0.9843 | 1.0030 | 1.0074 |
| buffer_callback | interp | 0.9360 | 0.9306 | 0.9618 | 0.9652 | 0.9474 |
| custom_root | jit | 0.9674 | 0.9243 | 0.9218 | 1.0038 | 0.9720 |
| custom_root | interp | 0.9796 | 0.9372 | 0.9605 | 0.9699 | 0.9857 |
| late_custom | jit | 0.9412 | 0.9788 | 0.9772 | 1.0046 | 0.9413 |
| late_custom | interp | 1.0123 | 1.0241 | 1.0087 | 0.9696 | 1.0175 |

Encoding with explicit options, ratios against CPython:

| Probe | Mode | Work time | Process time | Process CPU | Peak RSS | Work CPU |
|---|---|---:|---:|---:|---:|---:|
| none_p0 | jit | 63.5709 | 6.4406 | 6.8603 | 2.3731 | 64.3013 |
| none_p0 | interp | 59.6256 | 5.9695 | 6.3690 | 2.1764 | 59.9399 |
| none_p5 | jit | 10.2992 | 2.8047 | 2.9707 | 2.3442 | 10.4691 |
| none_p5 | interp | 10.2891 | 2.6217 | 2.7736 | 2.1628 | 10.2852 |
| none_default | jit | 10.1686 | 2.7163 | 2.8589 | 2.3337 | 10.1695 |
| none_default | interp | 9.8915 | 2.5787 | 2.7172 | 2.1619 | 9.8873 |
| list_p2 | jit | 166.5001 | 14.2156 | 15.1007 | 2.4057 | 164.7814 |
| list_p2 | interp | 149.1763 | 12.4928 | 13.6472 | 2.1876 | 148.8054 |
| list_p5 | jit | 11.4315 | 2.8078 | 2.9550 | 2.3503 | 11.3832 |
| list_p5 | interp | 10.6194 | 2.6274 | 2.7593 | 2.1592 | 10.6266 |
| integer_fix_imports | jit | 164.5767 | 13.8262 | 15.2768 | 2.4118 | 163.1794 |
| integer_fix_imports | interp | 161.3388 | 13.6206 | 14.7637 | 2.1900 | 160.9066 |
| custom_fix_imports | jit | 158.0994 | 14.6170 | 15.6794 | 2.3849 | 163.2238 |
| custom_fix_imports | interp | 154.8384 | 13.8180 | 14.7304 | 2.1640 | 154.1638 |
| buffer_callback | jit | 178.7895 | 14.5149 | 15.7158 | 2.4055 | 177.4637 |
| buffer_callback | interp | 167.0314 | 13.5701 | 14.8603 | 2.1919 | 166.3257 |
| custom_root | jit | 138.2382 | 19.0483 | 20.5478 | 2.3784 | 137.6659 |
| custom_root | interp | 127.8042 | 17.2211 | 18.4907 | 2.1592 | 127.4852 |
| late_custom | jit | 1045.2967 | 15.1469 | 16.5637 | 2.3138 | 1050.3913 |
| late_custom | interp | 946.5697 | 13.7382 | 14.8302 | 2.1315 | 952.0931 |

Ordinary calls, ratios against the preceding release:

| Probe | Mode | Work time | Process time | Process CPU | Peak RSS | Work CPU |
|---|---|---:|---:|---:|---:|---:|
| none_plain | jit | 0.0958 | 0.3231 | 0.3161 | 0.9784 | 0.0965 |
| none_plain | interp | 0.0963 | 0.3337 | 0.3264 | 0.9561 | 0.0961 |
| list_plain | jit | 0.0563 | 0.1914 | 0.1839 | 0.9818 | 0.0563 |
| list_plain | interp | 0.0525 | 0.1791 | 0.1787 | 0.9488 | 0.0525 |

Ordinary calls, ratios against CPython:

| Probe | Mode | Work time | Process time | Process CPU | Peak RSS | Work CPU |
|---|---|---:|---:|---:|---:|---:|
| none_plain | jit | 9.6289 | 2.6443 | 2.7933 | 2.3541 | 9.5613 |
| none_plain | interp | 8.3978 | 2.5210 | 2.6519 | 2.1655 | 8.3591 |
| list_plain | jit | 8.8810 | 2.5582 | 2.6823 | 2.3431 | 8.8025 |
| list_plain | interp | 8.6705 | 2.5021 | 2.6285 | 2.1592 | 8.6092 |

Decoding controls, ratios against the preceding release:

| Probe | Mode | Work time | Process time | Process CPU | Peak RSS | Work CPU |
|---|---|---:|---:|---:|---:|---:|
| none_p0 | jit | 1.0036 | 1.0169 | 1.0114 | 1.0043 | 1.0028 |
| none_p0 | interp | 1.0335 | 0.9941 | 1.0000 | 0.9694 | 1.0337 |
| none_p5 | jit | 0.9659 | 0.9967 | 1.0012 | 1.0026 | 0.9626 |
| none_p5 | interp | 1.0060 | 0.9451 | 0.9434 | 0.9660 | 1.0035 |
| list_p2 | jit | 1.0557 | 1.1173 | 1.1182 | 1.0060 | 1.0554 |
| list_p2 | interp | 1.0923 | 1.0500 | 1.0520 | 0.9698 | 1.0917 |
| list_p5 | jit | 0.9997 | 1.0382 | 1.0340 | 1.0026 | 0.9979 |
| list_p5 | interp | 1.0072 | 0.9925 | 0.9918 | 0.9674 | 1.0107 |
| list_encoding | jit | 1.0452 | 1.0283 | 1.0292 | 1.0073 | 1.0494 |
| list_encoding | interp | 0.9733 | 0.9546 | 0.9546 | 0.9672 | 0.9719 |
| list_buffers | jit | 1.0182 | 1.0523 | 1.0474 | 1.0013 | 1.0191 |
| list_buffers | interp | 0.9729 | 0.9735 | 0.9755 | 0.9725 | 0.9731 |

Decoding controls, ratios against CPython:

| Probe | Mode | Work time | Process time | Process CPU | Peak RSS | Work CPU |
|---|---|---:|---:|---:|---:|---:|
| none_p0 | jit | 204.3088 | 7.1760 | 7.8347 | 2.3863 | 204.5239 |
| none_p0 | interp | 198.7976 | 6.9343 | 7.6144 | 2.1826 | 198.7608 |
| none_p5 | jit | 18.7893 | 2.6615 | 2.8183 | 2.3534 | 18.7653 |
| none_p5 | interp | 16.7596 | 2.4118 | 2.5608 | 2.1752 | 16.7412 |
| list_p2 | jit | 239.5909 | 11.9601 | 12.8345 | 2.3912 | 237.6226 |
| list_p2 | interp | 239.9753 | 11.2998 | 12.2228 | 2.1801 | 239.8370 |
| list_p5 | jit | 17.2379 | 2.7896 | 2.9232 | 2.3569 | 17.2243 |
| list_p5 | interp | 15.5457 | 2.5875 | 2.7294 | 2.1704 | 15.5822 |
| list_encoding | jit | 297.5072 | 14.0887 | 15.4602 | 2.4016 | 297.4927 |
| list_encoding | interp | 267.3537 | 12.9597 | 14.1360 | 2.1850 | 267.1323 |
| list_buffers | jit | 278.6680 | 14.4966 | 15.2680 | 2.3937 | 278.3348 |
| list_buffers | interp | 251.6276 | 13.3565 | 14.3427 | 2.1818 | 251.9555 |

Ordinary None and small-list calls improve about 10 and 18 times in JIT mode, but still take about 9.6 and 8.9 times CPython workload time. The native guard adds measured fallback costs: legacy None is about 5 percent slower, and non-Boolean fix_imports cases take about 4 to 11 percent longer. The late-custom-object control retains its complete raw samples. Decoder legacy-list controls take about 6 to 9 percent longer. These regressions are retained, not excluded from the assessment.

## Full census

Five paired samples compare checkpoint 9a69c41, the preceding decoder release, and CPython. Geometric means include all 24 fixtures for process metrics. Startup is excluded only from the 23-fixture isolated workload timer aggregate.

| Metric | JIT/checkpoint | Interpreter/checkpoint | JIT/previous | Interpreter/previous | JIT/CPython | Interpreter/CPython |
|---|---:|---:|---:|---:|---:|---:|
| Workload time, 23 fixtures | 0.8011 | 0.9458 | 0.9824 | 0.9895 | 3.4976 | 9.7516 |
| Process time, 24 fixtures | 0.8527 | 0.9555 | 0.9765 | 0.9924 | 3.1478 | 5.6756 |
| CPU time, 24 fixtures | 0.8519 | 0.9552 | 0.9777 | 0.9895 | 3.2662 | 5.9461 |
| Peak RSS, 24 fixtures | 0.9369 | 0.9287 | 0.9928 | 0.9978 | 2.0761 | 1.9065 |

WeavePy wins 6/23 JIT workload timers and 0/24 JIT peak-RSS comparisons. The objective remains unachieved.

| Workload | JIT time/previous | Interpreter time/previous | JIT RSS/previous | JIT time/CPython | JIT RSS/CPython |
|---|---:|---:|---:|---:|---:|
| fannkuch | 0.9948 | 0.9767 | 0.9956 | 8.8995 | 1.9622 |
| nbody | 1.0046 | 1.0253 | 0.9989 | 9.2111 | 1.9577 |
| fib | 0.9974 | 0.9574 | 0.9967 | 3.0564 | 1.9816 |
| pidigits | 1.0066 | 1.0785 | 0.9871 | 0.9064 | 1.9702 |
| pyaes | 1.0057 | 0.9975 | 0.9984 | 0.6638 | 1.9660 |
| richards | 0.9985 | 0.9865 | 0.9951 | 8.3559 | 1.9687 |
| sumvm | 0.9755 | 0.9931 | 0.9967 | 0.0548 | 1.9783 |
| nested_loops | 1.0055 | 0.9885 | 0.9989 | 0.0921 | 1.9860 |
| jitloop | 0.9983 | 1.0054 | 0.9978 | 0.0757 | 1.9903 |
| jitkernels | 1.0029 | 1.0339 | 0.9898 | 0.8764 | 1.9638 |
| deltablue | 0.9737 | 1.0355 | 0.9991 | 20.9331 | 2.1577 |
| float_math | 0.9977 | 1.0074 | 1.0008 | 7.8812 | 2.9914 |
| spectral_norm | 0.9984 | 0.9933 | 0.9979 | 2.2461 | 1.9839 |
| json_bench | 1.0431 | 1.0181 | 0.9339 | 1.1721 | 2.5344 |
| str_methods | 0.9848 | 0.9831 | 1.0000 | 2.1100 | 2.1694 |
| dict_ops | 0.9887 | 0.9542 | 0.9967 | 5.2924 | 1.9478 |
| list_ops | 0.9721 | 1.0209 | 0.9945 | 14.5828 | 1.9496 |
| attr_access | 1.0068 | 0.9854 | 1.0005 | 2.6713 | 2.0205 |
| call_overhead | 0.9712 | 0.9196 | 0.9974 | 7.0302 | 2.0247 |
| generators | 0.9723 | 1.0568 | 0.9989 | 10.0222 | 1.9850 |
| deque_ops | 1.0915 | 1.0306 | 0.9997 | 17.5435 | 2.0069 |
| datetime_ops | 1.0058 | 1.0250 | 1.0000 | 159.2573 | 2.1273 |
| pickle_bench | 0.6727 | 0.7402 | 0.9595 | 204.6721 | 2.5287 |
| startup | 0.9924 | 1.0124 | 0.9956 | 1.3670 | 1.9674 |

## Controlled startup and imports

Thirty-one paired samples use JIT-disabled execution, equal-length executable paths, isolated caches, and verified matching or relocated serialized code filenames. Frozen artifacts remain unchanged throughout timing. Both pickle import orders are measured.

| Case | Elapsed/previous | CPU/previous | RSS/previous | Elapsed/CPython | CPU/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|---:|
| startup_matched | 0.9988 | 1.0007 | 0.9964 | 1.3320 | 1.3846 | 1.8244 |
| startup_relocated | 0.9939 | 1.0022 | 0.9976 | 1.3407 | 1.3739 | 1.8230 |
| no_site_matched | 1.0067 | 1.0097 | 1.0042 | 0.6059 | 0.5814 | 1.5532 |
| imports_matched | 0.9965 | 1.0016 | 0.9996 | 2.7287 | 2.8974 | 2.4306 |
| imports_relocated | 0.9921 | 0.9985 | 1.0004 | 2.7651 | 2.9125 | 2.4320 |
| pickle_import_matched | 0.9996 | 0.9994 | 1.0000 | 2.1388 | 2.2634 | 2.2258 |
| pickle_import_relocated | 0.9899 | 0.9860 | 0.9995 | 2.2119 | 2.3336 | 2.2264 |
| accelerator_first_matched | 1.0021 | 1.0008 | 0.9981 | 2.1611 | 2.2900 | 2.2183 |
| accelerator_first_relocated | 1.0048 | 1.0039 | 0.9986 | 2.1648 | 2.2884 | 2.2237 |

## History and limits

The initial integration failure and diagnostic rerun are preserved with their exact sources. They exposed the existing import-order bug described above. After that fix, the focused integration and full build checks passed. The original sandbox socket failures and successful authorized retries also remain in the archive.

The source snapshot identifies the exact encoder that produced these results. Later experiments must use a new identity and measurement directory. Energy, controlled build latency, parallel throughput, GC-pause distributions, and every Python workload are not measured here. GIL-disabled correctness does not establish free-threaded performance. The executable-size increase, memory tradeoffs, fallback regressions, and remaining CPython gaps prevent an overall performance-superiority claim.

Aggregate workload time falls about 1.76 percent with JIT enabled and 1.05 percent in interpreted mode versus the preceding release. Process elapsed time falls about 2.35 and 0.76 percent, respectively; peak RSS falls about 0.72 and 0.22 percent. The full pickle fixture takes about 33 percent less JIT workload time and 26 percent less interpreted time, but still takes about 205 times CPython JIT-comparison workload time. Deque JIT timing regresses about 9.1 percent; interpreted pidigits and generators regress about 7.9 and 5.7 percent. Those controls have no implementation changes in this stage. Their measurements remain part of the aggregate without assuming all movements are noise.
