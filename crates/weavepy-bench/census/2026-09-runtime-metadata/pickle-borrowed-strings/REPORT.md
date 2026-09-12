# Reduce pickle allocation and copying

Borrowed UTF-8 pickle strings now allocate their shared string storage directly. The preceding release actually performed two allocations, two copies, and a free in its compiled borrowed-string constructor. Disassembly of this candidate shows one allocation and one copy. Memo aliases and UTF-8 validation remain unchanged. The rejected direct-byte-allocation experiment is not included.

This complete census also includes the preceding retained bounded-list encoder change: list traversal reuses a snapshot of at most 1,000 items, releases the container lock before recursive encoding, and falls back on observed size changes. Strong memo pins, alias preservation, and cycle/depth guards remain in place. See the separate bounded-list archive for its focused gains and regressions.

The focused comparisons below isolate string construction against the bounded-list release 763d9a56. The full census and controlled startup comparisons use the latest preceding complete release f4630431, before both allocation changes. These two comparison baselines must not be conflated.

All 335 VM tests, Clippy, both import orders, and all 275 compatibility checks pass on the new executable. The compatibility suite runs outside the tool filesystem sandbox, including its loopback fixtures. The 1,266-case encoder differential accepts 642 cases natively per execution mode, with no regressions or CPython value differences. The 1,546-stream decoder differential accepts 282 streams per mode, with no regressions and 58 existing error-wording differences per mode. The 240-case recursion differential has no regressions and retains 82 existing CPython outcome differences per mode. Modes are JIT enabled, interpreted, and GIL disabled. Compiler/JIT/workspace/no-JIT checks last ran at f4630431 and were not repeated for these VM-only intermediate changes.

The executable SHA-256 is `e910f2dd2f1d87427f7613de9c304b42a3d1b87d8b2b0823c102f32d348c132b`, 44,289,056 bytes, the same file size as both comparison releases. The frozen inputs contain 132 sources and 37 measurement inputs plus supplemental scripts. No object layout source changes or new unsafe code are introduced by this stage; the 12 layout measurements from f4630431 were not repeated.

Timing uses the recorded CPython 3.14.7 GIL build outside the tool filesystem sandbox, with alternating paired order and one discarded preparation cycle. Ratios below one mean lower time or memory. Process elapsed time, CPU, and peak RSS come from OS measurements; workload CPU is recorded separately.

## Large string decoding

Four Unicode storage widths use approximately 8 MiB of UTF-8 input: ASCII, Latin-1, BMP, and non-BMP characters. Single-result and eight-result batches retain decoded strings until calculating their length checksum. The checksum is inside the workload timer. Separate verification processes check exact types, values, distinct identities across decodes, and native acceptance before and after workload-sized warmup. This is not an isolated parser timer.

Single-result peak RSS falls about 11 to 12 percent against 763d9a56; retained batch peak RSS falls about 6 percent. ASCII workload time falls about 9 to 16 percent, while multibyte-string improvements are smaller. Every case remains slower than CPython. The non-BMP retained batch uses less peak RSS than CPython; the other string cases still use more. These results do not establish a general Unicode memory advantage.

### large-controls: ratios against 763d9a56

| Probe | Mode | Work time | Process time | Process CPU | Peak RSS | Work CPU |
|---|---|---:|---:|---:|---:|---:|
| ascii_single | jit | 0.8793 | 0.9955 | 0.9954 | 0.8869 | 0.8797 |
| ascii_single | interp | 0.9096 | 0.9773 | 0.9797 | 0.8820 | 0.9098 |
| ascii_batch8 | jit | 0.8605 | 0.9726 | 0.9722 | 0.9369 | 0.8604 |
| ascii_batch8 | interp | 0.8394 | 0.9457 | 0.9573 | 0.9354 | 0.8394 |
| latin1_single | jit | 0.9868 | 0.9864 | 0.9848 | 0.8879 | 0.9870 |
| latin1_single | interp | 0.9847 | 0.9833 | 0.9841 | 0.8817 | 0.9792 |
| latin1_batch8 | jit | 0.9799 | 0.9839 | 0.9837 | 0.9372 | 0.9798 |
| latin1_batch8 | interp | 0.9811 | 0.9841 | 0.9832 | 0.9357 | 0.9811 |
| bmp_single | jit | 0.9821 | 0.9946 | 0.9948 | 0.8869 | 0.9820 |
| bmp_single | interp | 0.9833 | 0.9783 | 0.9791 | 0.8826 | 0.9834 |
| bmp_batch8 | jit | 0.9824 | 0.9894 | 0.9906 | 0.9378 | 0.9825 |
| bmp_batch8 | interp | 0.9817 | 0.9810 | 0.9806 | 0.9356 | 0.9816 |
| nonbmp_single | jit | 0.9794 | 0.9934 | 0.9935 | 0.8879 | 0.9795 |
| nonbmp_single | interp | 0.9709 | 0.9798 | 0.9815 | 0.8819 | 0.9709 |
| nonbmp_batch8 | jit | 0.9750 | 0.9833 | 0.9840 | 0.9375 | 0.9749 |
| nonbmp_batch8 | interp | 0.9753 | 0.9806 | 0.9791 | 0.9356 | 0.9753 |

### large-controls: ratios against CPython

| Probe | Mode | Work time | Process time | Process CPU | Peak RSS | Work CPU |
|---|---|---:|---:|---:|---:|---:|
| ascii_single | jit | 2.8077 | 2.0888 | 2.2029 | 1.9687 | 2.8124 |
| ascii_single | interp | 2.7888 | 2.0105 | 2.1194 | 1.8917 | 2.7972 |
| ascii_batch8 | jit | 1.8689 | 1.9611 | 2.0476 | 1.3463 | 1.8693 |
| ascii_batch8 | interp | 1.8689 | 1.8895 | 1.9683 | 1.3194 | 1.8686 |
| latin1_single | jit | 2.1264 | 2.0969 | 2.1801 | 1.9658 | 2.1270 |
| latin1_single | interp | 2.1098 | 2.0413 | 2.1284 | 1.8875 | 2.1093 |
| latin1_batch8 | jit | 2.0378 | 2.0841 | 2.1136 | 1.3429 | 2.0379 |
| latin1_batch8 | interp | 2.0357 | 2.0592 | 2.0899 | 1.3160 | 2.0357 |
| bmp_single | jit | 2.4847 | 2.2258 | 2.3106 | 1.8058 | 2.4864 |
| bmp_single | interp | 2.4859 | 2.1542 | 2.2553 | 1.7350 | 2.4871 |
| bmp_batch8 | jit | 2.3985 | 2.3488 | 2.3976 | 1.0748 | 2.3986 |
| bmp_batch8 | interp | 2.4086 | 2.3263 | 2.3734 | 1.0529 | 2.4087 |
| nonbmp_single | jit | 1.8639 | 2.0786 | 2.1691 | 1.5652 | 1.8649 |
| nonbmp_single | interp | 1.8641 | 2.0066 | 2.0962 | 1.5042 | 1.8661 |
| nonbmp_batch8 | jit | 1.7605 | 1.8783 | 1.9155 | 0.7750 | 1.7605 |
| nonbmp_batch8 | interp | 1.7625 | 1.8643 | 1.9026 | 0.7594 | 1.7625 |

### focused: ratios against 763d9a56

| Probe | Mode | Work time | Process time | Process CPU | Peak RSS | Work CPU |
|---|---|---:|---:|---:|---:|---:|
| payload_dumps | jit | 1.0048 | 1.0026 | 1.0092 | 1.0056 | 1.0049 |
| payload_dumps | interp | 0.9791 | 0.9688 | 0.9709 | 1.0023 | 0.9784 |
| payload_loads | jit | 0.9791 | 0.9830 | 0.9864 | 1.0030 | 0.9788 |
| payload_loads | interp | 0.9059 | 1.0050 | 1.0124 | 0.9972 | 0.9127 |
| records_dumps | jit | 0.9885 | 0.9811 | 0.9875 | 0.9979 | 0.9866 |
| records_dumps | interp | 1.0134 | 1.0010 | 0.9959 | 1.0018 | 1.0130 |
| records_loads | jit | 0.9801 | 0.9937 | 1.0024 | 0.9996 | 0.9970 |
| records_loads | interp | 0.9956 | 1.0079 | 1.0098 | 1.0036 | 0.9951 |
| large_int_list_loads | jit | 0.9985 | 1.0052 | 1.0084 | 1.0029 | 0.9988 |
| large_int_list_loads | interp | 0.9997 | 0.9934 | 0.9965 | 0.9992 | 0.9994 |
| large_int_list_dumps | jit | 1.0007 | 0.9952 | 0.9962 | 1.0035 | 1.0008 |
| large_int_list_dumps | interp | 1.0012 | 0.9875 | 0.9885 | 1.0007 | 1.0019 |
| large_int_tuple_loads | jit | 0.9966 | 1.0058 | 1.0061 | 1.0026 | 0.9961 |
| large_int_tuple_loads | interp | 0.9929 | 0.9942 | 0.9929 | 1.0009 | 0.9932 |
| large_int_tuple_dumps | jit | 1.0012 | 1.0038 | 1.0027 | 1.0014 | 1.0030 |
| large_int_tuple_dumps | interp | 0.9977 | 0.9825 | 0.9815 | 0.9997 | 0.9974 |
| large_dicts_loads | jit | 0.9459 | 0.9881 | 0.9864 | 1.0047 | 0.9477 |
| large_dicts_loads | interp | 0.9148 | 0.9760 | 0.9760 | 1.0018 | 0.9148 |
| large_dicts_dumps | jit | 0.9926 | 0.9993 | 1.0000 | 1.0028 | 0.9924 |
| large_dicts_dumps | interp | 0.9804 | 0.9852 | 0.9866 | 1.0026 | 0.9804 |
| large_bytes_loads | jit | 0.9794 | 1.0099 | 1.0131 | 1.0023 | 0.9820 |
| large_bytes_loads | interp | 0.9593 | 0.9880 | 0.9914 | 1.0009 | 0.9595 |
| large_bytes_dumps | jit | 0.8951 | 1.0120 | 1.0119 | 1.0018 | 0.8910 |
| large_bytes_dumps | interp | 1.0356 | 0.9850 | 0.9845 | 1.0002 | 1.0362 |

### focused: ratios against CPython

| Probe | Mode | Work time | Process time | Process CPU | Peak RSS | Work CPU |
|---|---|---:|---:|---:|---:|---:|
| payload_dumps | jit | 3.2289 | 2.2191 | 2.3447 | 2.3203 | 3.2343 |
| payload_dumps | interp | 3.1991 | 2.1126 | 2.2267 | 2.1563 | 3.2634 |
| payload_loads | jit | 4.5645 | 2.2711 | 2.3567 | 2.3046 | 4.5594 |
| payload_loads | interp | 4.4018 | 2.0990 | 2.2339 | 2.1461 | 4.4167 |
| records_dumps | jit | 319.2749 | 29.5234 | 32.0880 | 2.4000 | 317.4536 |
| records_dumps | interp | 296.7721 | 26.7406 | 29.0291 | 2.2118 | 294.2433 |
| records_loads | jit | 311.9191 | 19.4265 | 20.9966 | 2.4118 | 312.6481 |
| records_loads | interp | 267.8981 | 16.6722 | 17.9605 | 2.2094 | 269.0495 |
| large_int_list_loads | jit | 0.4935 | 1.7960 | 1.8635 | 1.8090 | 0.4931 |
| large_int_list_loads | interp | 0.4927 | 1.7172 | 1.7849 | 1.7081 | 0.4925 |
| large_int_list_dumps | jit | 1.5234 | 1.8603 | 1.9243 | 1.7627 | 1.5236 |
| large_int_list_dumps | interp | 1.5582 | 1.7777 | 1.8457 | 1.6671 | 1.5590 |
| large_int_tuple_loads | jit | 0.7738 | 1.8644 | 1.9424 | 2.1160 | 0.7734 |
| large_int_tuple_loads | interp | 0.7825 | 1.7908 | 1.8610 | 2.0249 | 0.7825 |
| large_int_tuple_dumps | jit | 1.6899 | 1.8935 | 1.9697 | 2.1465 | 1.6900 |
| large_int_tuple_dumps | interp | 1.7128 | 1.8463 | 1.9191 | 2.0552 | 1.7168 |
| large_dicts_loads | jit | 3.3182 | 2.4767 | 2.6103 | 2.2310 | 3.3206 |
| large_dicts_loads | interp | 3.2152 | 2.3251 | 2.4380 | 2.0836 | 3.2157 |
| large_dicts_dumps | jit | 2.9726 | 2.2552 | 2.3593 | 2.1293 | 2.9749 |
| large_dicts_dumps | interp | 2.9566 | 2.1551 | 2.2600 | 2.0245 | 2.9574 |
| large_bytes_loads | jit | 1.3483 | 2.0573 | 2.1735 | 1.7118 | 1.3457 |
| large_bytes_loads | interp | 1.3491 | 1.9992 | 2.1201 | 1.6355 | 1.3522 |
| large_bytes_dumps | jit | 0.7849 | 2.0696 | 2.1886 | 1.4712 | 0.7849 |
| large_bytes_dumps | interp | 0.8408 | 1.9810 | 2.0919 | 1.4195 | 0.8405 |

### controls: ratios against 763d9a56

| Probe | Mode | Work time | Process time | Process CPU | Peak RSS | Work CPU |
|---|---|---:|---:|---:|---:|---:|
| none_p0 | jit | 1.0192 | 1.0420 | 1.0436 | 1.0052 | 1.0160 |
| none_p0 | interp | 1.0568 | 1.0092 | 1.0062 | 0.9968 | 1.0554 |
| none_p5 | jit | 0.9987 | 0.9928 | 1.0195 | 1.0039 | 0.9986 |
| none_p5 | interp | 1.0428 | 0.9698 | 0.9940 | 0.9977 | 0.9281 |
| none_default | jit | 0.9893 | 1.0332 | 0.9947 | 1.0061 | 0.9880 |
| none_default | interp | 0.9539 | 0.9974 | 1.0288 | 0.9995 | 1.0237 |
| list_p2 | jit | 1.0151 | 1.0088 | 1.0133 | 1.0030 | 1.0185 |
| list_p2 | interp | 0.9961 | 0.9875 | 1.0055 | 0.9968 | 1.0133 |
| list_p5 | jit | 1.0115 | 1.0032 | 1.0034 | 1.0022 | 1.0116 |
| list_p5 | interp | 0.9993 | 1.0179 | 1.0034 | 1.0014 | 0.9781 |
| integer_fix_imports | jit | 0.9906 | 1.0046 | 1.0044 | 1.0047 | 0.9978 |
| integer_fix_imports | interp | 1.0155 | 1.0041 | 1.0074 | 1.0009 | 1.0144 |
| custom_fix_imports | jit | 1.0316 | 1.0049 | 0.9985 | 1.0008 | 1.0314 |
| custom_fix_imports | interp | 1.0281 | 1.0044 | 1.0096 | 0.9977 | 1.0282 |
| buffer_callback | jit | 1.0518 | 1.0258 | 1.0235 | 1.0038 | 1.0418 |
| buffer_callback | interp | 1.0055 | 1.0082 | 0.9941 | 1.0023 | 0.9978 |
| custom_root | jit | 1.0270 | 1.0242 | 1.0216 | 1.0008 | 1.0220 |
| custom_root | interp | 1.0135 | 1.0185 | 1.0179 | 1.0014 | 1.0176 |
| late_custom | jit | 1.0000 | 0.9937 | 0.9960 | 1.0000 | 0.9979 |
| late_custom | interp | 1.0035 | 0.9968 | 0.9927 | 0.9955 | 1.0011 |

### controls: ratios against CPython

| Probe | Mode | Work time | Process time | Process CPU | Peak RSS | Work CPU |
|---|---|---:|---:|---:|---:|---:|
| none_p0 | jit | 68.5944 | 6.4126 | 6.8010 | 2.3563 | 68.1203 |
| none_p0 | interp | 66.0792 | 6.1393 | 6.5277 | 2.1734 | 65.4597 |
| none_p5 | jit | 10.9505 | 2.7934 | 2.8480 | 2.3471 | 10.9730 |
| none_p5 | interp | 10.8262 | 2.5485 | 2.3587 | 2.1528 | 10.8090 |
| none_default | jit | 14.2820 | 2.6728 | 2.6976 | 2.3270 | 13.3945 |
| none_default | interp | 15.2527 | 2.5800 | 2.6502 | 2.1567 | 16.4772 |
| list_p2 | jit | 162.2849 | 14.4522 | 15.4874 | 2.4000 | 162.9817 |
| list_p2 | interp | 158.3983 | 13.6632 | 14.6577 | 2.1884 | 158.2443 |
| list_p5 | jit | 11.6670 | 2.9947 | 3.1574 | 2.3489 | 11.6368 |
| list_p5 | interp | 10.7938 | 2.9023 | 3.0517 | 2.1750 | 10.9429 |
| integer_fix_imports | jit | 173.8257 | 16.1925 | 17.3569 | 2.3949 | 174.2686 |
| integer_fix_imports | interp | 161.2988 | 14.7282 | 16.0521 | 2.1939 | 161.5611 |
| custom_fix_imports | jit | 167.4830 | 16.2474 | 17.5151 | 2.3731 | 167.5248 |
| custom_fix_imports | interp | 153.1258 | 14.8595 | 15.9547 | 2.1633 | 153.1687 |
| buffer_callback | jit | 187.2056 | 16.6307 | 17.5635 | 2.4000 | 185.4204 |
| buffer_callback | interp | 173.8203 | 15.2633 | 16.1243 | 2.1996 | 173.5386 |
| custom_root | jit | 146.0374 | 20.7641 | 22.3165 | 2.3624 | 145.4133 |
| custom_root | interp | 133.9662 | 19.3926 | 20.7513 | 2.1637 | 134.5679 |
| late_custom | jit | 1032.1116 | 15.5728 | 16.8240 | 2.3103 | 1032.3610 |
| late_custom | interp | 929.2869 | 14.3468 | 15.3451 | 2.1394 | 937.6098 |

### decode-controls: ratios against 763d9a56

| Probe | Mode | Work time | Process time | Process CPU | Peak RSS | Work CPU |
|---|---|---:|---:|---:|---:|---:|
| none_p0 | jit | 0.9988 | 1.0017 | 1.0024 | 1.0022 | 0.9987 |
| none_p0 | interp | 0.9912 | 0.9895 | 0.9897 | 0.9991 | 0.9912 |
| none_p5 | jit | 0.9732 | 1.0104 | 1.0113 | 1.0057 | 0.9732 |
| none_p5 | interp | 0.9940 | 0.9983 | 0.9953 | 1.0019 | 0.9936 |
| list_p2 | jit | 1.0013 | 1.0045 | 1.0060 | 1.0009 | 1.0013 |
| list_p2 | interp | 1.0091 | 1.0030 | 1.0031 | 1.0028 | 1.0086 |
| list_p5 | jit | 1.0078 | 1.0205 | 1.0202 | 1.0039 | 1.0077 |
| list_p5 | interp | 0.9927 | 1.0051 | 1.0043 | 1.0028 | 0.9928 |
| list_encoding | jit | 1.0062 | 1.0051 | 1.0046 | 1.0000 | 1.0062 |
| list_encoding | interp | 1.0013 | 1.0013 | 1.0020 | 1.0019 | 1.0014 |
| list_buffers | jit | 1.0134 | 1.0121 | 1.0120 | 1.0009 | 1.0133 |
| list_buffers | interp | 1.0197 | 1.0170 | 1.0177 | 1.0000 | 1.0197 |

### decode-controls: ratios against CPython

| Probe | Mode | Work time | Process time | Process CPU | Peak RSS | Work CPU |
|---|---|---:|---:|---:|---:|---:|
| none_p0 | jit | 192.5088 | 8.0046 | 8.6442 | 2.3670 | 195.2891 |
| none_p0 | interp | 184.7115 | 7.5011 | 8.1252 | 2.1850 | 185.6342 |
| none_p5 | jit | 18.8278 | 2.6820 | 2.8636 | 2.3609 | 18.9068 |
| none_p5 | interp | 16.5078 | 2.5254 | 2.6699 | 2.1867 | 16.5628 |
| list_p2 | jit | 245.9835 | 12.4634 | 13.5529 | 2.3706 | 246.1818 |
| list_p2 | interp | 233.5705 | 11.7878 | 12.7975 | 2.1862 | 233.9760 |
| list_p5 | jit | 17.1101 | 2.8724 | 3.0481 | 2.3520 | 17.1225 |
| list_p5 | interp | 15.9373 | 2.7213 | 2.8702 | 2.1803 | 16.0197 |
| list_encoding | jit | 267.8445 | 14.3796 | 15.5806 | 2.3824 | 268.6229 |
| list_encoding | interp | 254.6131 | 13.5427 | 14.6732 | 2.1847 | 255.2195 |
| list_buffers | jit | 271.3940 | 14.8164 | 16.1347 | 2.3855 | 271.3732 |
| list_buffers | interp | 257.5598 | 13.8833 | 15.0463 | 2.1961 | 258.3428 |

### plain-controls: ratios against 763d9a56

| Probe | Mode | Work time | Process time | Process CPU | Peak RSS | Work CPU |
|---|---|---:|---:|---:|---:|---:|
| none_plain | jit | 1.0143 | 1.0151 | 1.0162 | 1.0048 | 1.0139 |
| none_plain | interp | 1.0039 | 0.9872 | 0.9855 | 1.0005 | 1.0047 |
| list_plain | jit | 1.0048 | 1.0127 | 1.0101 | 1.0035 | 1.0047 |
| list_plain | interp | 0.9934 | 0.9860 | 0.9858 | 0.9986 | 0.9933 |

### plain-controls: ratios against CPython

| Probe | Mode | Work time | Process time | Process CPU | Peak RSS | Work CPU |
|---|---|---:|---:|---:|---:|---:|
| none_plain | jit | 8.7901 | 2.7086 | 2.8654 | 2.3537 | 8.7998 |
| none_plain | interp | 8.0439 | 2.5157 | 2.6710 | 2.1784 | 8.0536 |
| list_plain | jit | 9.1570 | 2.6904 | 2.8430 | 2.3445 | 9.1610 |
| list_plain | interp | 8.5124 | 2.5095 | 2.6532 | 2.1717 | 8.5108 |

Large nested-dictionary decoding takes about 5 to 9 percent less workload time than 763d9a56; integer-list encoding and decoding are nearly unchanged. Small configured encoder controls retain regressions: interpreted legacy None takes about 5.7 percent longer and JIT buffer-callback calls about 5.2 percent longer. Their encoder implementations did not change in this string stage. All favorable and unfavorable results remain recorded, without assuming that unrelated movements are entirely noise.

## Full census

Five paired samples compare checkpoint 9a69c41, preceding complete release f4630431, and CPython. Geometric means include all 24 workloads for process metrics and exclude startup only from the 23-workload isolated timer aggregate.

| Metric | JIT/checkpoint | Interpreter/checkpoint | JIT/f463 | Interpreter/f463 | JIT/CPython | Interpreter/CPython |
|---|---:|---:|---:|---:|---:|---:|
| Workload time, 23 workloads | 0.8038 | 0.9427 | 1.0065 | 0.9975 | 3.4706 | 9.4814 |
| Process time, 24 workloads | 0.8588 | 0.9545 | 0.9968 | 1.0016 | 3.2093 | 5.7428 |
| CPU time, 24 workloads | 0.8558 | 0.9541 | 0.9975 | 1.0010 | 3.3055 | 5.9542 |
| Peak RSS, 24 workloads | 0.9391 | 0.9328 | 0.9973 | 1.0062 | 2.0791 | 1.9150 |

WeavePy wins 6/23 JIT workload timers and 0/24 JIT peak-RSS comparisons. The overall goal remains unachieved.

| Workload | JIT time/f463 | Interpreter time/f463 | JIT RSS/f463 | Interpreter RSS/f463 | JIT time/CPython | JIT RSS/CPython |
|---|---:|---:|---:|---:|---:|---:|
| fannkuch | 1.0296 | 1.0109 | 1.0033 | 1.0113 | 9.5352 | 1.9676 |
| nbody | 1.0102 | 0.9919 | 1.0043 | 1.0070 | 9.4001 | 1.9683 |
| fib | 1.0229 | 1.0183 | 1.0016 | 1.0071 | 3.1400 | 1.9924 |
| pidigits | 1.0015 | 1.0047 | 0.9990 | 0.9955 | 0.9102 | 1.9575 |
| pyaes | 0.9935 | 0.9825 | 1.0027 | 1.0095 | 0.6474 | 1.9766 |
| richards | 1.0060 | 1.0034 | 1.0038 | 1.0083 | 8.4337 | 1.9742 |
| sumvm | 1.0008 | 0.9957 | 1.0066 | 1.0095 | 0.0566 | 1.9892 |
| nested_loops | 1.0001 | 0.9804 | 1.0049 | 1.0047 | 0.0829 | 1.9935 |
| jitloop | 0.9970 | 0.9930 | 1.0022 | 1.0059 | 0.0725 | 1.9989 |
| jitkernels | 1.0037 | 1.0089 | 1.0032 | 1.0065 | 0.9121 | 1.9818 |
| deltablue | 0.9872 | 1.0083 | 0.9960 | 1.0041 | 19.2958 | 2.1541 |
| float_math | 0.9708 | 1.0050 | 1.0006 | 1.0026 | 7.1187 | 2.9909 |
| spectral_norm | 1.0081 | 1.0481 | 1.0032 | 1.0065 | 2.3113 | 1.9968 |
| json_bench | 1.0348 | 1.0171 | 0.9392 | 1.0075 | 1.2791 | 2.5210 |
| str_methods | 0.9891 | 0.9989 | 1.0055 | 1.0041 | 2.0357 | 2.1847 |
| dict_ops | 1.0012 | 0.9770 | 1.0038 | 1.0094 | 5.4589 | 1.9615 |
| list_ops | 1.0043 | 1.0025 | 1.0066 | 1.0053 | 13.6945 | 1.9603 |
| attr_access | 1.0333 | 0.9735 | 1.0059 | 1.0077 | 2.6197 | 2.0236 |
| call_overhead | 0.9839 | 0.9830 | 1.0048 | 1.0047 | 8.4492 | 2.0290 |
| generators | 1.0199 | 0.9985 | 1.0043 | 1.0083 | 8.7243 | 1.9882 |
| deque_ops | 1.0695 | 0.9638 | 1.0014 | 1.0047 | 16.5489 | 2.0062 |
| datetime_ops | 0.9974 | 0.9829 | 1.0015 | 1.0067 | 152.6894 | 2.1262 |
| pickle_bench | 0.9906 | 0.9980 | 0.9305 | 1.0054 | 205.4063 | 2.4545 |
| startup | 0.9866 | 1.0231 | 1.0050 | 1.0065 | 1.3732 | 1.9740 |

## Controlled startup and imports

Thirty-one paired samples use JIT-disabled execution, equal-length executable paths, isolated caches, and verified matching or relocated serialized code filenames. Frozen artifacts remain unchanged during measurement. Both pickle import orders are covered.

| Case | Elapsed/f463 | CPU/f463 | RSS/f463 | Elapsed/CPython | CPU/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|---:|
| startup_matched | 1.0013 | 1.0033 | 1.0084 | 1.3670 | 1.4101 | 1.8366 |
| startup_relocated | 1.0063 | 1.0009 | 1.0090 | 1.3798 | 1.4256 | 1.8386 |
| no_site_matched | 1.0146 | 1.0056 | 1.0008 | 0.6252 | 0.5915 | 1.5580 |
| imports_matched | 0.9991 | 1.0003 | 1.0017 | 2.7668 | 2.9377 | 2.4310 |
| imports_relocated | 0.9980 | 0.9987 | 1.0013 | 2.7952 | 2.9829 | 2.4335 |
| pickle_import_matched | 1.0010 | 1.0020 | 1.0057 | 2.2547 | 2.3763 | 2.2363 |
| pickle_import_relocated | 0.9973 | 0.9982 | 1.0057 | 2.2095 | 2.3386 | 2.2340 |
| accelerator_first_matched | 0.9976 | 0.9978 | 1.0071 | 2.2029 | 2.3464 | 2.2319 |
| accelerator_first_relocated | 0.9984 | 0.9922 | 1.0061 | 2.2025 | 2.3184 | 2.2363 |

Aggregate workload time rises about 0.65 percent with JIT enabled and falls about 0.25 percent in interpreted mode against f4630431. Process elapsed time falls about 0.32 percent with JIT enabled and rises about 0.16 percent in interpreted mode. Peak RSS falls about 0.27 percent with JIT enabled but rises about 0.62 percent in interpreted mode. The full pickle workload takes about 205 times CPython workload time. Deque JIT workload time regresses about 7 percent. These aggregate and control regressions remain part of the assessment alongside the targeted allocation improvements.

Startup without site takes about 1.46 percent longer than f4630431. Other controlled startup elapsed ratios are near one; peak-RSS ratios range from 1.00085 to 1.00896. Neither the focused memory gains nor the unchanged executable size imply that every startup or broader workload improves.

## Limits

Energy, controlled build latency, parallel throughput, GC-pause distributions, and every Python workload are not measured here. GIL-disabled correctness does not establish free-threaded performance. The remaining timing and memory gaps and retained regressions prevent an overall performance-superiority claim.
