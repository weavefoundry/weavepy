# Rejected direct byte-allocation trial

This trial replaced Object::new_bytes on borrowed pickle payloads with direct shared byte allocation, preserving the empty singleton. Source inspection suggested that the existing helper created an intermediate vector. The compiled release already eliminates that intermediate allocation and copy, so the rewrite is not retained.

All 335 VM tests, Clippy, both import orders, and the encoder/recursion/decoder differentials pass. Native acceptance and existing CPython differences remain unchanged. The full compatibility suite, full census, and controlled startup comparisons were prepared but not run after the focused results failed to establish a useful targeted improvement.

The candidate SHA-256 is `74d3dff922d1bceda31c43037a0b6b18df3cf90a926bc479e63d1dd3cffc78d1`, 44,288,768 bytes, 288 bytes smaller than the preceding bounded-list release. It is preserved only as an experimental binary identity. The retained runtime returns to the bounded-list implementation; subsequent experiments need their own snapshots.

Seven paired samples of 8 MiB decoding show JIT workload time at 0.9895 times the preceding release and interpreted time at 1.1948 times. Peak RSS is essentially unchanged in both modes. Byte-encoding workload time, whose implementation did not change, rises about 13 percent with JIT enabled and 8 percent in interpreted mode. All controls are retained below, including favorable movements.

## Allocation evidence

The first instrumented run set both MallocStackLogging=1 and MallocStackLoggingNoCompact=1. The installed system manuals establish that the former takes precedence and selects live-allocation-only mode. Its counts cannot answer the temporary-allocation question. The entire initial captures remain losslessly compressed, with original sizes and SHA-256 hashes.

The corrected run unsets MallocStackLogging and sets only MallocStackLoggingNoCompact=1. Each binary performs one native-helper decode and one public decode of an 8 MiB payload. Both histories show two large decoder allocations of 8,388,624 bytes, one per decode. Relevant allocation blocks, on-disk logs, command lines, and hashes of the full verbose streams are retained. Unrelated verbose full-stream stacks are not stored.

Disassembly confirms that the preceding nonempty byte branch calls malloc once, initializes shared reference counts, and calls memcpy once. The candidate does the same. Both assembly listings and exact stub mappings are retained. This is diagnostic evidence, not a timing or peak-RSS measurement.

The borrowed-string helper was inspected separately. Its existing assembly still contains two allocation and copy calls plus a free. That is a new follow-up lead, not a measured string performance result.

## Focused measurements

All timing runs are outside the tool filesystem sandbox, with alternating paired order and one discarded preparation cycle. Below one means less time or memory. Process CPU, elapsed time, and peak RSS are actual OS measurements.

### focused: ratios against 763d bounded-list release

| Probe | Mode | Work time | Process time | Process CPU | Peak RSS | Work CPU |
|---|---|---:|---:|---:|---:|---:|
| payload_dumps | jit | 1.0058 | 1.0121 | 1.0129 | 1.0000 | 1.0066 |
| payload_dumps | interp | 0.9876 | 0.9825 | 0.9807 | 1.0028 | 0.9885 |
| payload_loads | jit | 0.9729 | 1.0192 | 1.0135 | 1.0013 | 0.9720 |
| payload_loads | interp | 0.9945 | 0.9964 | 0.9959 | 1.0009 | 0.9940 |
| records_dumps | jit | 0.9850 | 0.9710 | 0.9699 | 1.0008 | 0.9850 |
| records_dumps | interp | 0.9974 | 0.9931 | 0.9924 | 0.9982 | 0.9975 |
| records_loads | jit | 1.0224 | 1.0059 | 1.0067 | 1.0025 | 1.0220 |
| records_loads | interp | 1.0098 | 1.0165 | 1.0165 | 0.9991 | 1.0099 |
| large_int_list_loads | jit | 0.9528 | 1.0070 | 1.0079 | 1.0011 | 0.9529 |
| large_int_list_loads | interp | 0.9863 | 0.9680 | 0.9655 | 0.9989 | 0.9858 |
| large_int_list_dumps | jit | 1.0352 | 1.0428 | 1.0441 | 0.9993 | 1.0358 |
| large_int_list_dumps | interp | 0.9942 | 1.0130 | 1.0094 | 0.9993 | 0.9935 |
| large_int_tuple_loads | jit | 0.9749 | 0.9886 | 0.9842 | 1.0003 | 0.9751 |
| large_int_tuple_loads | interp | 0.9989 | 0.9581 | 0.9563 | 1.0015 | 0.9984 |
| large_int_tuple_dumps | jit | 0.9604 | 1.0048 | 1.0081 | 1.0000 | 0.9607 |
| large_int_tuple_dumps | interp | 1.0085 | 1.0007 | 0.9984 | 1.0003 | 1.0085 |
| large_dicts_loads | jit | 1.0100 | 1.0045 | 1.0049 | 1.0010 | 1.0102 |
| large_dicts_loads | interp | 0.9860 | 0.9860 | 0.9859 | 1.0011 | 0.9862 |
| large_dicts_dumps | jit | 0.9946 | 1.0027 | 1.0042 | 1.0012 | 0.9947 |
| large_dicts_dumps | interp | 1.0286 | 1.0253 | 1.0219 | 1.0000 | 1.0287 |
| large_bytes_loads | jit | 0.9895 | 1.0022 | 1.0030 | 0.9994 | 0.9842 |
| large_bytes_loads | interp | 1.1948 | 0.9865 | 0.9847 | 0.9994 | 1.1806 |
| large_bytes_dumps | jit | 1.1336 | 1.0055 | 0.9977 | 1.0016 | 1.1334 |
| large_bytes_dumps | interp | 1.0766 | 0.9854 | 0.9873 | 1.0002 | 1.0765 |

### focused: ratios against CPython

| Probe | Mode | Work time | Process time | Process CPU | Peak RSS | Work CPU |
|---|---|---:|---:|---:|---:|---:|
| payload_dumps | jit | 3.2521 | 2.2327 | 2.3526 | 2.3113 | 3.2730 |
| payload_dumps | interp | 3.1518 | 2.0862 | 2.2011 | 2.1590 | 3.1560 |
| payload_loads | jit | 4.5298 | 2.1786 | 2.2935 | 2.2986 | 4.5361 |
| payload_loads | interp | 4.5517 | 2.0794 | 2.2015 | 2.1419 | 4.5706 |
| records_dumps | jit | 306.9417 | 26.7292 | 29.4721 | 2.3994 | 306.5450 |
| records_dumps | interp | 284.5968 | 25.1577 | 27.4227 | 2.2030 | 285.0535 |
| records_loads | jit | 332.4264 | 18.1364 | 19.8165 | 2.4133 | 333.7317 |
| records_loads | interp | 280.4432 | 16.4942 | 17.8576 | 2.2028 | 281.1147 |
| large_int_list_loads | jit | 0.4669 | 1.8667 | 1.8676 | 1.8042 | 0.4666 |
| large_int_list_loads | interp | 0.4810 | 1.7327 | 1.7768 | 1.7056 | 0.4804 |
| large_int_list_dumps | jit | 1.5168 | 1.8115 | 1.8504 | 1.7602 | 1.5171 |
| large_int_list_dumps | interp | 1.5344 | 1.7462 | 1.8032 | 1.6669 | 1.5338 |
| large_int_tuple_loads | jit | 0.8972 | 1.9800 | 2.0255 | 2.1160 | 0.8969 |
| large_int_tuple_loads | interp | 0.8377 | 1.8305 | 1.8685 | 2.0262 | 0.8376 |
| large_int_tuple_dumps | jit | 1.6426 | 1.9102 | 1.9644 | 2.1502 | 1.6426 |
| large_int_tuple_dumps | interp | 1.6831 | 1.8489 | 1.9007 | 2.0607 | 1.6822 |
| large_dicts_loads | jit | 3.2473 | 2.3487 | 2.4515 | 2.2273 | 3.2493 |
| large_dicts_loads | interp | 3.1848 | 2.2807 | 2.3670 | 2.0800 | 3.1857 |
| large_dicts_dumps | jit | 2.9756 | 2.1930 | 2.2735 | 2.1265 | 2.9762 |
| large_dicts_dumps | interp | 3.1116 | 2.1312 | 2.2036 | 2.0245 | 3.1144 |
| large_bytes_loads | jit | 1.3366 | 2.0069 | 2.1097 | 1.7088 | 1.3345 |
| large_bytes_loads | interp | 1.2374 | 1.9415 | 2.0434 | 1.6348 | 1.2230 |
| large_bytes_dumps | jit | 1.5040 | 2.0302 | 2.1168 | 1.4678 | 1.5063 |
| large_bytes_dumps | interp | 1.0786 | 1.9691 | 2.0271 | 1.4178 | 1.0791 |

### controls: ratios against 763d bounded-list release

| Probe | Mode | Work time | Process time | Process CPU | Peak RSS | Work CPU |
|---|---|---:|---:|---:|---:|---:|
| none_p0 | jit | 1.0082 | 0.9952 | 1.0100 | 1.0004 | 1.0065 |
| none_p0 | interp | 1.0021 | 0.9978 | 0.9984 | 0.9995 | 1.0021 |
| none_p5 | jit | 0.9811 | 1.0200 | 1.0143 | 1.0013 | 0.9812 |
| none_p5 | interp | 0.9869 | 0.9891 | 0.9910 | 1.0014 | 0.9871 |
| none_default | jit | 1.0119 | 1.0089 | 1.0092 | 1.0013 | 1.0120 |
| none_default | interp | 0.9874 | 0.9949 | 0.9937 | 1.0005 | 0.9874 |
| list_p2 | jit | 0.9941 | 0.9945 | 1.0028 | 1.0021 | 1.0000 |
| list_p2 | interp | 1.0287 | 1.0134 | 1.0127 | 1.0000 | 1.0274 |
| list_p5 | jit | 1.0087 | 1.0061 | 1.0090 | 1.0022 | 1.0109 |
| list_p5 | interp | 0.9972 | 0.9984 | 0.9977 | 1.0000 | 0.9974 |
| integer_fix_imports | jit | 1.0394 | 1.0173 | 1.0172 | 0.9996 | 1.0385 |
| integer_fix_imports | interp | 1.0318 | 1.0259 | 1.0257 | 1.0000 | 1.0314 |
| custom_fix_imports | jit | 1.0133 | 1.0253 | 1.0265 | 1.0004 | 1.0130 |
| custom_fix_imports | interp | 1.0440 | 1.0320 | 1.0320 | 0.9982 | 1.0440 |
| buffer_callback | jit | 1.0440 | 1.0126 | 1.0125 | 1.0051 | 1.0440 |
| buffer_callback | interp | 1.0287 | 1.0152 | 1.0148 | 1.0005 | 1.0280 |
| custom_root | jit | 1.0346 | 1.0285 | 1.0288 | 0.9992 | 1.0345 |
| custom_root | interp | 1.0217 | 1.0187 | 1.0197 | 1.0009 | 1.0217 |
| late_custom | jit | 0.9975 | 1.0256 | 1.0212 | 1.0017 | 0.9975 |
| late_custom | interp | 1.0073 | 0.9902 | 0.9889 | 0.9982 | 1.0080 |

### controls: ratios against CPython

| Probe | Mode | Work time | Process time | Process CPU | Peak RSS | Work CPU |
|---|---|---:|---:|---:|---:|---:|
| none_p0 | jit | 62.4259 | 6.4498 | 6.9342 | 2.3631 | 62.3977 |
| none_p0 | interp | 57.4037 | 5.9262 | 6.4326 | 2.1753 | 57.4917 |
| none_p5 | jit | 10.7381 | 2.7977 | 2.9560 | 2.3330 | 10.7549 |
| none_p5 | interp | 9.7711 | 2.5775 | 2.7519 | 2.1623 | 9.7849 |
| none_default | jit | 9.9395 | 2.7582 | 2.9021 | 2.3299 | 9.9527 |
| none_default | interp | 9.6662 | 2.6375 | 2.7768 | 2.1635 | 9.6788 |
| list_p2 | jit | 156.5264 | 13.6016 | 14.8053 | 2.3882 | 156.6988 |
| list_p2 | interp | 150.3223 | 12.7051 | 13.8665 | 2.1947 | 150.6089 |
| list_p5 | jit | 11.2318 | 2.8694 | 3.0745 | 2.3347 | 11.2302 |
| list_p5 | interp | 10.2871 | 2.7157 | 2.8763 | 2.1589 | 10.3014 |
| integer_fix_imports | jit | 175.4114 | 14.8597 | 16.1726 | 2.3974 | 175.4870 |
| integer_fix_imports | interp | 159.7487 | 14.0708 | 15.2330 | 2.1935 | 159.9167 |
| custom_fix_imports | jit | 163.5716 | 15.5707 | 17.0321 | 2.3582 | 163.5426 |
| custom_fix_imports | interp | 150.7038 | 14.2384 | 15.7961 | 2.1557 | 150.8174 |
| buffer_callback | jit | 178.2818 | 15.0017 | 16.1389 | 2.3974 | 178.5584 |
| buffer_callback | interp | 166.1179 | 14.0318 | 15.1953 | 2.1947 | 166.3695 |
| custom_root | jit | 138.8141 | 19.7905 | 21.4603 | 2.3671 | 138.9836 |
| custom_root | interp | 129.8467 | 17.9485 | 19.4301 | 2.1672 | 129.9257 |
| late_custom | jit | 1082.9062 | 15.6516 | 17.2981 | 2.3033 | 1097.9735 |
| late_custom | interp | 983.6949 | 13.7419 | 14.9459 | 2.1322 | 998.2556 |

### decode-controls: ratios against 763d bounded-list release

| Probe | Mode | Work time | Process time | Process CPU | Peak RSS | Work CPU |
|---|---|---:|---:|---:|---:|---:|
| none_p0 | jit | 1.0035 | 1.0161 | 1.0165 | 1.0017 | 1.0035 |
| none_p0 | interp | 1.0093 | 1.0029 | 1.0020 | 0.9995 | 1.0081 |
| none_p5 | jit | 0.9880 | 0.9753 | 0.9786 | 0.9996 | 0.9883 |
| none_p5 | interp | 0.9809 | 0.9422 | 0.9403 | 0.9995 | 0.9810 |
| list_p2 | jit | 0.9950 | 1.0052 | 1.0049 | 1.0000 | 0.9950 |
| list_p2 | interp | 1.0166 | 1.0034 | 1.0034 | 0.9981 | 1.0162 |
| list_p5 | jit | 1.0065 | 1.0000 | 1.0012 | 1.0009 | 1.0066 |
| list_p5 | interp | 0.9916 | 1.0033 | 1.0012 | 1.0019 | 0.9915 |
| list_encoding | jit | 1.0024 | 1.0090 | 1.0083 | 1.0000 | 1.0023 |
| list_encoding | interp | 1.0264 | 1.0180 | 1.0165 | 0.9995 | 1.0262 |
| list_buffers | jit | 1.0086 | 1.0126 | 1.0124 | 0.9979 | 1.0086 |
| list_buffers | interp | 1.0159 | 1.0074 | 1.0077 | 0.9977 | 1.0167 |

### decode-controls: ratios against CPython

| Probe | Mode | Work time | Process time | Process CPU | Peak RSS | Work CPU |
|---|---|---:|---:|---:|---:|---:|
| none_p0 | jit | 191.5173 | 7.6140 | 8.2899 | 2.3601 | 192.2104 |
| none_p0 | interp | 185.0867 | 7.0584 | 7.6788 | 2.1862 | 185.6583 |
| none_p5 | jit | 17.8362 | 2.6976 | 2.8644 | 2.3452 | 18.0670 |
| none_p5 | interp | 16.1013 | 2.4547 | 2.6157 | 2.1752 | 16.1646 |
| list_p2 | jit | 245.2936 | 12.2661 | 13.3051 | 2.3744 | 246.2803 |
| list_p2 | interp | 236.6876 | 11.7515 | 12.7680 | 2.1851 | 237.0820 |
| list_p5 | jit | 17.1621 | 2.8907 | 3.0602 | 2.3384 | 17.2190 |
| list_p5 | interp | 15.7447 | 2.6944 | 2.8588 | 2.1764 | 15.8153 |
| list_encoding | jit | 271.5584 | 12.6135 | 13.7954 | 2.3706 | 271.9179 |
| list_encoding | interp | 260.2872 | 12.8826 | 13.9861 | 2.1872 | 260.8547 |
| list_buffers | jit | 269.2255 | 14.4678 | 15.5836 | 2.3875 | 269.4598 |
| list_buffers | interp | 254.5074 | 13.1804 | 14.3421 | 2.1936 | 254.6959 |

### plain-controls: ratios against 763d bounded-list release

| Probe | Mode | Work time | Process time | Process CPU | Peak RSS | Work CPU |
|---|---|---:|---:|---:|---:|---:|
| none_plain | jit | 0.9949 | 1.0161 | 1.0158 | 1.0017 | 0.9949 |
| none_plain | interp | 1.0055 | 0.9899 | 0.9883 | 0.9995 | 1.0054 |
| list_plain | jit | 0.9993 | 0.9985 | 0.9979 | 1.0022 | 0.9995 |
| list_plain | interp | 1.0000 | 0.9947 | 0.9993 | 0.9991 | 0.9997 |

### plain-controls: ratios against CPython

| Probe | Mode | Work time | Process time | Process CPU | Peak RSS | Work CPU |
|---|---|---:|---:|---:|---:|---:|
| none_plain | jit | 9.1502 | 2.5884 | 2.7400 | 2.3391 | 9.1707 |
| none_plain | interp | 8.1837 | 2.4674 | 2.6014 | 2.1675 | 8.1844 |
| list_plain | jit | 9.3019 | 2.7313 | 2.8863 | 2.3340 | 9.3142 |
| list_plain | interp | 8.6974 | 2.5810 | 2.7194 | 2.1614 | 8.7075 |

The objective of outperforming CPython across every meaningful metric remains unachieved. Rejected experiments must not be counted as retained improvements.
