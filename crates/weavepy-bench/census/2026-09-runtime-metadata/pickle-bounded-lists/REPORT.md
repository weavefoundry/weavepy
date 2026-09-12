# Bound temporary list storage during pickle encoding

The encoder now copies at most 1,000 list elements into a reused temporary vector before recursively encoding each batch. It releases the container lock before recursion, keeps the original length as a bound, and returns to the existing pickler if a batch observes resizing. Memo pins and cycle/depth guards remain unchanged.

All 334 VM tests and Clippy checks pass. Two new tests verify shared children across 2,001 list entries and valid accepted streams during concurrent resizing. Both import orders, the 1,266-case encoder differential, 240-case recursion differential, and 1,546-stream decoder differential pass against the preceding f463 release. Native acceptance remains 642 encoder cases and 282 decoder cases per execution mode. Existing CPython recursion outcomes and malformed decoder wording differences remain unchanged.

The executable SHA-256 is `763d9a569a81027ba18abdc3ea3fa2e6af398657d5bd89391edc5e76125819f3`, 44,289,056 bytes, the same size as the preceding encoder. The snapshot preserves 132 sources and 37 measurement inputs plus supplemental scripts. No new unsafe code is introduced.

This is a measured intermediate. Compiler/JIT/workspace/no-JIT checks passed at f463 and were not rerun here. The complete 275-check compatibility suite, 24-workload census, and controlled startup comparison were not repeated for this intermediate. The latest complete census remains the preceding encoder archive.

Large integer-list encoding uses 9.4 percent less JIT peak RSS and 9.9 percent less interpreted peak RSS than f463, recovering nearly all of the prior snapshot-related memory increase. Workload time falls about 7.1 and 13.8 percent, respectively. Whole-process JIT elapsed time still rises about 0.8 percent. CPython remains faster and smaller on this encoding case: JIT-comparison workload time is 1.76 times CPython and peak RSS is 1.76 times CPython.

The first seven-cycle explicit-option small-list comparison shows an 11.7 percent JIT slowdown. A 15-cycle repeat with 20,000 calls instead of 2,000 does not reproduce it: that case improves about 4.2 percent, and ordinary list calls remain within about 1 percent of f463. The repeat instead retains a 6.8 percent JIT slowdown for explicitly configured None calls. Both complete experiments remain below; neither replaces the other. Untouched byte and decoder paths also have unfavorable movements, which are not attributed entirely to this change or to noise.

All measurements run outside the tool filesystem sandbox against CPython 3.14.7, with alternating paired order and one discarded cycle. Ratios below one mean less time or memory. Process CPU, elapsed time, and peak RSS are actual OS measurements.

## focused: ratios against f463

7 paired samples.

| Probe | Mode | Work time | Process time | Process CPU | Peak RSS | Work CPU |
|---|---|---:|---:|---:|---:|---:|
| payload_dumps | jit | 0.9457 | 1.0034 | 1.0077 | 1.0009 | 0.9510 |
| payload_dumps | interp | 0.9796 | 0.9999 | 0.9995 | 1.0028 | 0.9790 |
| payload_loads | jit | 1.0120 | 1.0091 | 1.0084 | 1.0000 | 1.0117 |
| payload_loads | interp | 1.1142 | 1.0069 | 0.9963 | 1.0005 | 1.0964 |
| records_dumps | jit | 0.9601 | 1.0261 | 1.0231 | 0.9996 | 0.9658 |
| records_dumps | interp | 1.0328 | 1.0573 | 1.0541 | 1.0009 | 1.0270 |
| records_loads | jit | 1.1517 | 1.1113 | 1.1083 | 1.0000 | 1.1479 |
| records_loads | interp | 0.9659 | 1.0466 | 1.0478 | 1.0036 | 0.9689 |
| large_int_list_loads | jit | 1.0078 | 1.0404 | 1.0209 | 1.0004 | 1.0079 |
| large_int_list_loads | interp | 1.0487 | 1.0198 | 1.0222 | 1.0011 | 1.0484 |
| large_int_list_dumps | jit | 0.9294 | 1.0085 | 1.0054 | 0.9062 | 0.9273 |
| large_int_list_dumps | interp | 0.8619 | 0.9573 | 0.9470 | 0.9009 | 0.8591 |
| large_int_tuple_loads | jit | 0.9801 | 1.0134 | 1.0134 | 0.9989 | 0.9724 |
| large_int_tuple_loads | interp | 1.0256 | 1.0028 | 0.9982 | 1.0021 | 1.0460 |
| large_int_tuple_dumps | jit | 0.9952 | 1.0015 | 0.9997 | 1.0000 | 0.9926 |
| large_int_tuple_dumps | interp | 1.0132 | 0.9851 | 0.9916 | 1.0012 | 1.0122 |
| large_dicts_loads | jit | 0.9840 | 0.9876 | 0.9874 | 1.0007 | 0.9816 |
| large_dicts_loads | interp | 0.9955 | 1.0027 | 1.0011 | 0.9996 | 0.9998 |
| large_dicts_dumps | jit | 1.0204 | 1.0148 | 1.0054 | 1.0019 | 1.0075 |
| large_dicts_dumps | interp | 0.9121 | 0.9814 | 0.9819 | 1.0016 | 0.9180 |
| large_bytes_loads | jit | 1.1436 | 1.0481 | 1.0380 | 1.0017 | 1.1447 |
| large_bytes_loads | interp | 0.8578 | 1.0002 | 0.9911 | 1.0015 | 0.8551 |
| large_bytes_dumps | jit | 1.0185 | 1.0182 | 1.0231 | 1.0009 | 1.0214 |
| large_bytes_dumps | interp | 1.1308 | 0.9997 | 0.9994 | 0.9993 | 1.1272 |

## focused: ratios against CPython

7 paired samples.

| Probe | Mode | Work time | Process time | Process CPU | Peak RSS | Work CPU |
|---|---|---:|---:|---:|---:|---:|
| payload_dumps | jit | 3.3541 | 2.1497 | 2.2433 | 2.3170 | 3.3687 |
| payload_dumps | interp | 3.2401 | 2.0169 | 2.1160 | 2.1565 | 3.2480 |
| payload_loads | jit | 4.2935 | 2.1406 | 2.2148 | 2.2941 | 4.3112 |
| payload_loads | interp | 4.5474 | 2.0869 | 2.1670 | 2.1382 | 4.5688 |
| records_dumps | jit | 309.9111 | 25.6677 | 27.5234 | 2.4066 | 306.9346 |
| records_dumps | interp | 296.2258 | 24.0127 | 27.1471 | 2.2094 | 294.7823 |
| records_loads | jit | 347.2686 | 17.7596 | 19.3159 | 2.4188 | 352.9613 |
| records_loads | interp | 270.5673 | 15.5927 | 16.9602 | 2.2128 | 274.4726 |
| large_int_list_loads | jit | 0.4973 | 1.7651 | 1.7702 | 1.8023 | 0.4970 |
| large_int_list_loads | interp | 0.4766 | 1.6607 | 1.7139 | 1.7045 | 0.4763 |
| large_int_list_dumps | jit | 1.7589 | 1.7987 | 1.8871 | 1.7608 | 1.7506 |
| large_int_list_dumps | interp | 1.5698 | 1.7253 | 1.7869 | 1.6636 | 1.5720 |
| large_int_tuple_loads | jit | 0.7983 | 1.7954 | 1.8609 | 2.1266 | 0.7981 |
| large_int_tuple_loads | interp | 0.8519 | 1.7670 | 1.8136 | 2.0371 | 0.8531 |
| large_int_tuple_dumps | jit | 1.7070 | 1.9050 | 1.9905 | 2.1585 | 1.7073 |
| large_int_tuple_dumps | interp | 1.7103 | 1.8194 | 1.9052 | 2.0677 | 1.7287 |
| large_dicts_loads | jit | 3.1762 | 2.2758 | 2.3703 | 2.2241 | 3.1597 |
| large_dicts_loads | interp | 2.9831 | 2.2194 | 2.3283 | 2.0806 | 2.9881 |
| large_dicts_dumps | jit | 3.0777 | 2.1596 | 2.3059 | 2.1197 | 3.0789 |
| large_dicts_dumps | interp | 3.0000 | 2.0044 | 2.1765 | 2.0231 | 3.0017 |
| large_bytes_loads | jit | 0.8998 | 1.9671 | 2.0528 | 1.7129 | 0.8978 |
| large_bytes_loads | interp | 0.9708 | 1.9224 | 2.0015 | 1.6351 | 0.9666 |
| large_bytes_dumps | jit | 0.9225 | 2.0394 | 2.0954 | 1.4687 | 0.9239 |
| large_bytes_dumps | interp | 1.0031 | 1.9382 | 1.9861 | 1.4191 | 1.0040 |

## controls: ratios against f463

7 paired samples.

| Probe | Mode | Work time | Process time | Process CPU | Peak RSS | Work CPU |
|---|---|---:|---:|---:|---:|---:|
| none_p0 | jit | 1.0028 | 1.1002 | 1.0297 | 0.9996 | 1.0026 |
| none_p0 | interp | 0.9997 | 0.9979 | 1.0003 | 1.0051 | 1.0013 |
| none_p5 | jit | 0.9894 | 0.9991 | 0.9979 | 1.0009 | 0.9864 |
| none_p5 | interp | 0.9975 | 0.9922 | 0.9882 | 0.9972 | 0.9961 |
| none_default | jit | 1.0408 | 1.0085 | 1.0089 | 0.9996 | 1.0382 |
| none_default | interp | 1.0116 | 0.9963 | 0.9959 | 0.9995 | 1.0065 |
| list_p2 | jit | 1.0078 | 1.0224 | 1.0247 | 0.9992 | 1.0098 |
| list_p2 | interp | 1.0106 | 0.9999 | 0.9998 | 1.0037 | 1.0082 |
| list_p5 | jit | 1.1174 | 1.0288 | 1.0226 | 0.9991 | 1.1178 |
| list_p5 | interp | 0.9780 | 0.9783 | 0.9767 | 1.0028 | 0.9812 |
| integer_fix_imports | jit | 0.9537 | 0.9341 | 0.9336 | 0.9966 | 0.9536 |
| integer_fix_imports | interp | 1.0043 | 1.0073 | 1.0085 | 1.0042 | 1.0050 |
| custom_fix_imports | jit | 1.0080 | 1.0127 | 1.0144 | 0.9941 | 1.0100 |
| custom_fix_imports | interp | 1.0362 | 1.0017 | 1.0025 | 1.0023 | 0.9916 |
| buffer_callback | jit | 1.0859 | 1.1275 | 1.1278 | 0.9966 | 1.0921 |
| buffer_callback | interp | 0.9838 | 0.9751 | 0.9752 | 1.0037 | 0.9845 |
| custom_root | jit | 1.0215 | 1.0246 | 1.0272 | 0.9992 | 1.0221 |
| custom_root | interp | 1.0373 | 1.0498 | 1.0292 | 1.0060 | 1.0358 |
| late_custom | jit | 0.9549 | 0.9098 | 0.9143 | 0.9992 | 0.9571 |
| late_custom | interp | 0.9739 | 0.9361 | 0.9595 | 1.0031 | 0.9764 |

## controls: ratios against CPython

7 paired samples.

| Probe | Mode | Work time | Process time | Process CPU | Peak RSS | Work CPU |
|---|---|---:|---:|---:|---:|---:|
| none_p0 | jit | 61.5560 | 6.3957 | 6.8966 | 2.3721 | 62.9072 |
| none_p0 | interp | 59.4727 | 5.9407 | 6.3934 | 2.1892 | 59.3555 |
| none_p5 | jit | 11.2182 | 2.7712 | 2.9424 | 2.3418 | 11.1798 |
| none_p5 | interp | 10.3220 | 2.5961 | 2.7134 | 2.1643 | 10.3103 |
| none_default | jit | 10.5638 | 2.7099 | 2.8648 | 2.3401 | 10.5480 |
| none_default | interp | 10.3196 | 2.6235 | 2.7980 | 2.1655 | 10.3259 |
| list_p2 | jit | 158.1598 | 13.3417 | 14.5378 | 2.3955 | 157.8085 |
| list_p2 | interp | 154.1292 | 12.3454 | 13.2139 | 2.2057 | 153.6316 |
| list_p5 | jit | 12.0272 | 3.0437 | 3.2146 | 2.3337 | 12.2579 |
| list_p5 | interp | 10.3630 | 2.7758 | 2.8698 | 2.1714 | 10.5516 |
| integer_fix_imports | jit | 165.4681 | 14.4310 | 15.7473 | 2.4006 | 165.2447 |
| integer_fix_imports | interp | 151.9451 | 13.5870 | 14.7359 | 2.2010 | 151.7645 |
| custom_fix_imports | jit | 168.6964 | 15.8144 | 17.1213 | 2.3614 | 168.7276 |
| custom_fix_imports | interp | 159.0916 | 14.3595 | 15.5256 | 2.1682 | 159.0536 |
| buffer_callback | jit | 197.4223 | 17.5957 | 19.1048 | 2.3984 | 197.1172 |
| buffer_callback | interp | 174.9443 | 13.7109 | 15.1688 | 2.2020 | 174.3692 |
| custom_root | jit | 143.4978 | 20.2301 | 22.2731 | 2.3657 | 143.2993 |
| custom_root | interp | 135.6231 | 18.3927 | 19.5528 | 2.1754 | 135.0685 |
| late_custom | jit | 1080.1802 | 16.5796 | 17.7630 | 2.3109 | 1081.7317 |
| late_custom | interp | 959.4541 | 14.0377 | 15.0529 | 2.1408 | 964.1134 |

## decode-controls: ratios against f463

7 paired samples.

| Probe | Mode | Work time | Process time | Process CPU | Peak RSS | Work CPU |
|---|---|---:|---:|---:|---:|---:|
| none_p0 | jit | 1.0099 | 0.9983 | 0.9976 | 0.9991 | 1.0091 |
| none_p0 | interp | 1.0044 | 1.0031 | 0.9987 | 1.0014 | 1.0063 |
| none_p5 | jit | 0.9944 | 0.9895 | 0.9918 | 1.0000 | 0.9960 |
| none_p5 | interp | 1.0247 | 1.0387 | 1.0025 | 1.0009 | 1.0190 |
| list_p2 | jit | 0.9717 | 0.9744 | 0.9745 | 1.0013 | 0.9843 |
| list_p2 | interp | 1.0062 | 1.0140 | 1.0104 | 1.0033 | 1.0059 |
| list_p5 | jit | 1.0041 | 1.0158 | 1.0090 | 1.0017 | 1.0035 |
| list_p5 | interp | 0.9830 | 0.9920 | 0.9928 | 1.0019 | 0.9854 |
| list_encoding | jit | 0.9714 | 0.9595 | 0.9566 | 0.9987 | 0.9626 |
| list_encoding | interp | 0.9569 | 0.9765 | 0.9730 | 1.0019 | 0.9545 |
| list_buffers | jit | 1.0685 | 1.0743 | 1.0699 | 0.9987 | 1.0646 |
| list_buffers | interp | 1.0436 | 1.0422 | 1.0440 | 1.0033 | 1.0444 |

## decode-controls: ratios against CPython

7 paired samples.

| Probe | Mode | Work time | Process time | Process CPU | Peak RSS | Work CPU |
|---|---|---:|---:|---:|---:|---:|
| none_p0 | jit | 193.9123 | 7.3037 | 7.9247 | 2.3684 | 193.4888 |
| none_p0 | interp | 185.6469 | 7.0068 | 7.5438 | 2.1951 | 185.3036 |
| none_p5 | jit | 18.2605 | 2.5619 | 2.7067 | 2.3463 | 18.2554 |
| none_p5 | interp | 16.6119 | 2.4974 | 2.6045 | 2.1729 | 16.5781 |
| list_p2 | jit | 240.9918 | 11.1062 | 12.0397 | 2.3855 | 241.5542 |
| list_p2 | interp | 238.7285 | 10.4066 | 11.7203 | 2.1896 | 241.6476 |
| list_p5 | jit | 17.5304 | 2.7589 | 2.9487 | 2.3537 | 17.4071 |
| list_p5 | interp | 15.7106 | 2.6125 | 2.7796 | 2.1814 | 15.6842 |
| list_encoding | jit | 293.0307 | 13.5251 | 14.7776 | 2.3865 | 290.7308 |
| list_encoding | interp | 268.1466 | 12.1419 | 13.2001 | 2.1973 | 267.3239 |
| list_buffers | jit | 300.6729 | 13.6155 | 14.6322 | 2.3792 | 294.7929 |
| list_buffers | interp | 256.1949 | 12.2925 | 13.5534 | 2.1957 | 255.9600 |

## plain-controls: ratios against f463

7 paired samples.

| Probe | Mode | Work time | Process time | Process CPU | Peak RSS | Work CPU |
|---|---|---:|---:|---:|---:|---:|
| none_plain | jit | 1.0250 | 1.0328 | 1.0296 | 1.0022 | 1.0162 |
| none_plain | interp | 1.0126 | 0.9847 | 0.9937 | 1.0014 | 1.0127 |
| list_plain | jit | 1.0033 | 1.0091 | 1.0143 | 0.9996 | 1.0079 |
| list_plain | interp | 1.0037 | 0.9861 | 0.9932 | 1.0005 | 1.0046 |

## plain-controls: ratios against CPython

7 paired samples.

| Probe | Mode | Work time | Process time | Process CPU | Peak RSS | Work CPU |
|---|---|---:|---:|---:|---:|---:|
| none_plain | jit | 9.0659 | 2.6564 | 2.7976 | 2.3462 | 9.0054 |
| none_plain | interp | 8.0527 | 2.4655 | 2.5914 | 2.1646 | 8.0533 |
| list_plain | jit | 9.6273 | 2.6756 | 2.8154 | 2.3340 | 9.6134 |
| list_plain | interp | 8.9808 | 2.5323 | 2.6508 | 2.1682 | 8.9321 |

## repeat-explicit: ratios against f463

15 paired samples.

| Probe | Mode | Work time | Process time | Process CPU | Peak RSS | Work CPU |
|---|---|---:|---:|---:|---:|---:|
| none_p5 | jit | 1.0676 | 1.0693 | 1.0669 | 1.0000 | 1.0567 |
| none_p5 | interp | 0.9975 | 1.0006 | 0.9981 | 0.9995 | 0.9989 |
| list_p5 | jit | 0.9575 | 0.9769 | 0.9869 | 0.9987 | 0.9635 |
| list_p5 | interp | 0.9733 | 0.9378 | 0.9380 | 1.0037 | 0.9615 |

## repeat-explicit: ratios against CPython

15 paired samples.

| Probe | Mode | Work time | Process time | Process CPU | Peak RSS | Work CPU |
|---|---|---:|---:|---:|---:|---:|
| none_p5 | jit | 11.3603 | 5.9865 | 6.3512 | 2.3408 | 11.1866 |
| none_p5 | interp | 9.8952 | 5.4338 | 5.6977 | 2.1684 | 9.8357 |
| list_p5 | jit | 11.3006 | 6.1152 | 6.3915 | 2.3398 | 11.3235 |
| list_p5 | interp | 10.3637 | 5.6176 | 5.8257 | 2.1740 | 10.3424 |

## repeat-plain: ratios against f463

15 paired samples.

| Probe | Mode | Work time | Process time | Process CPU | Peak RSS | Work CPU |
|---|---|---:|---:|---:|---:|---:|
| none_plain | jit | 1.0041 | 0.9934 | 0.9935 | 0.9996 | 1.0061 |
| none_plain | interp | 0.9640 | 0.9587 | 0.9617 | 1.0019 | 0.9630 |
| list_plain | jit | 0.9925 | 0.9939 | 0.9975 | 1.0009 | 0.9929 |
| list_plain | interp | 1.0053 | 0.9891 | 0.9886 | 1.0019 | 1.0055 |

## repeat-plain: ratios against CPython

15 paired samples.

| Probe | Mode | Work time | Process time | Process CPU | Peak RSS | Work CPU |
|---|---|---:|---:|---:|---:|---:|
| none_plain | jit | 8.9378 | 4.8351 | 5.0661 | 2.3425 | 8.9213 |
| none_plain | interp | 8.0751 | 4.4458 | 4.5932 | 2.1717 | 8.0531 |
| list_plain | jit | 9.4072 | 5.1586 | 5.3673 | 2.3445 | 9.3965 |
| list_plain | interp | 8.6580 | 4.8149 | 5.0269 | 2.1726 | 8.6499 |

Energy, controlled build latency, and free-threaded performance are not measured. GIL-disabled correctness does not establish parallel performance. The objective of outperforming CPython across every meaningful metric remains unachieved.
