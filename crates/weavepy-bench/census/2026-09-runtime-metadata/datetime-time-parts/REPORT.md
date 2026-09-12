# Parse supported ISO time components natively

The Python datetime implementation now parses supported ASCII time components through a stateless Rust helper. It accepts hour, minute, and second forms with consistent separators, plus second fractions with six or more digits. It verifies the whole fraction before truncating to six digits and returns a fresh mutable list of four integer components. It avoids repeated Python slices, integer conversions, and ASCII-digit callbacks.

Short fractions retain the existing correction-table path. Unsupported inputs, Unicode, and string subclasses return to the existing parser. Range validation, timezone handling, midnight rollover, and public construction remain in the existing implementation. The native helper does not invoke Python callbacks for accepted exact strings; arbitrary replacement of private parser globals is not universally guarded.

The paired isolated datetime ISO component takes about 60 percent less workload time than e910f2dd. Six- and nine-digit fractional-time controls improve roughly four to six times. Their measured peak-RSS reductions are about 1 to 2 percent. Short-fraction fallbacks take about 1 to 2 percent longer. Unchanged timedelta multiplication retains a 4.2 percent JIT workload regression. All favorable and unfavorable results are recorded.

The full datetime fixture remains effectively unchanged in JIT workload time (.999 relative to e910f2dd) and takes 2.1 percent longer interpreted. It still takes about 153 times CPython JIT workload time. Across the full suite, JIT workload time falls 0.4 percent and interpreted time rises 1.1 percent. The 14.4 percent nested-loop JIT improvement occurs in unchanged code; no causal parser improvement is claimed for that control. String-method interpreted time rises 4.1 percent. Controlled no-site startup takes 3.3 percent longer. These are measured tradeoffs, not evidence of across-the-board improvement.

## Validation

All 337 VM tests passed. Clippy initially rejected a test literal without digit separators; changing 123456 to 123_456 fixed the style issue. Clippy then passed and the CLI release built successfully. The unchanged VM tests were not repeated after that formatting-only fix. All 275 compatibility checks passed in one run outside the tool filesystem sandbox, including loopback and threading fixtures. Compiler/JIT/workspace/no-JIT checks were not repeated for this VM and Python-module change.

The targeted Python test passes in JIT, interpreted, and GIL-disabled modes. It checks native list identity and mutability, accepted formats, fallback shapes, subclass callback order, replacement of short-fraction correction values, and public time/datetime subclasses. Separate instrumentation after 2,000 warm calls proves that public ISO entry points reach the helper and that short fractions fall back.

A 5,139-case differential preserves every preceding outcome in all three modes. It covers public time/datetime and private component parsing. Each mode accepts 390 private helper cases natively, all matching the preceding helper exactly. Existing CPython differences remain: 39 success/error outcomes, 577 error messages, and three string-subclass callback patterns per mode. These differences are not new and must not be hidden by a claim of complete CPython compatibility.

A separate 175-case low-recursion differential has nine CPython outcome convergences across the three modes. Its exact comparison also flags six RecursionError wording changes: the candidate gets farther before failing in __instancecheck__. Both WeavePy builds raise RecursionError in those cases while CPython succeeds or raises ValueError. No previously successful result or exception type regresses. The exact-comparison failure is retained unchanged, alongside a separate assessment; this is not an all-pass exact-message differential.

The executable SHA-256 is `21b59ded060c615b5f5ea7995b2f6e0a20ec01ef0aa556c34f0fc8626e42cff8`, 44,289,200 bytes, 144 bytes larger than e910f2dd. The release snapshot captures 134 sources and 37 initial measurement inputs plus supplemental scripts. No new unsafe code or object-layout source changes are introduced. The preceding layout measurements were not repeated.

## Cache-isolation correction

The initial paired component run used the default shared frozen cache. The preceding and new builds embed different _pydatetime.py sources, but the cache stores one file per module name. Alternation invalidated the file and charged recompilation to the next build. Raw peak RSS increased by about 8 MiB in alternating variants, creating an apparent interpreter memory gain of about 22 percent. That is not a demonstrated parser memory saving. The original run, original harness, and diagnosis remain intact.

The corrected runs give each executable its own frozen cache, shared between its JIT and interpreted modes. Hashes taken after preparation and after measured cycles remain unchanged. This removes the alternating recompilation penalty. Corrected measurement-input snapshots supersede the shared-cache harness in the release snapshot without changing the runtime binary. The full census uses the new --frozen-cache-root option. A missing startup log directory caused one initial full-run launch to fail before any measurements; the failure and corrected launch are preserved.

Earlier cross-build runs without cache isolation can include recompilation when embedded Python sources differ. Their raw records remain historical measurements with that limitation. The separate controlled startup studies already used isolated caches. This observation does not invalidate every historical workload timer or comparisons with identical embedded sources.

## Isolated components and public parsing controls

Each probe uses seven paired samples after a discarded cycle, outside the tool filesystem sandbox, against e910f2dd and the recorded CPython 3.14.7 GIL build. Work is 2,000 operations. Separate processes verify canonical values or error types before and after warmup in every mode. Full error messages remain in the differential. Process elapsed time, CPU, and peak RSS are OS measurements; workload CPU is recorded separately. Below one means less time or memory.

### components-isolated: ratios against e910f2dd

| Probe | Mode | Work time | Process time | Process CPU | Peak RSS | Work CPU |
|---|---|---:|---:|---:|---:|---:|
| datetime_construct | jit | 1.0006 | 1.0093 | 1.0087 | 1.0009 | 1.0006 |
| datetime_construct | interp | 1.0075 | 0.9995 | 1.0013 | 1.0006 | 1.0072 |
| timedelta_construct | jit | 1.0091 | 1.0064 | 1.0073 | 1.0024 | 1.0090 |
| timedelta_construct | interp | 0.9958 | 0.9992 | 1.0011 | 1.0006 | 0.9957 |
| datetime_add | jit | 1.0056 | 0.9988 | 1.0008 | 1.0005 | 1.0056 |
| datetime_add | interp | 1.0067 | 1.0045 | 1.0049 | 1.0011 | 1.0067 |
| datetime_subtract | jit | 1.0250 | 1.0182 | 1.0171 | 1.0014 | 1.0251 |
| datetime_subtract | interp | 1.0076 | 0.9912 | 0.9940 | 1.0022 | 1.0075 |
| timedelta_multiply | jit | 1.0422 | 1.0189 | 1.0201 | 1.0015 | 1.0419 |
| timedelta_multiply | interp | 1.0175 | 1.0104 | 1.0100 | 0.9994 | 1.0175 |
| datetime_fields | jit | 1.0057 | 1.0076 | 1.0060 | 1.0030 | 1.0053 |
| datetime_fields | interp | 0.9962 | 0.9927 | 0.9899 | 0.9972 | 0.9964 |
| datetime_weekday | jit | 1.0212 | 1.0006 | 1.0032 | 1.0000 | 1.0215 |
| datetime_weekday | interp | 1.0124 | 1.0003 | 0.9993 | 0.9972 | 1.0123 |
| datetime_isoformat | jit | 1.0147 | 1.0146 | 1.0144 | 1.0010 | 1.0147 |
| datetime_isoformat | interp | 1.0155 | 1.0169 | 1.0158 | 0.9994 | 1.0155 |
| datetime_strftime | jit | 1.0062 | 1.0041 | 1.0047 | 1.0031 | 1.0061 |
| datetime_strftime | interp | 1.0094 | 1.0056 | 1.0051 | 1.0039 | 1.0093 |
| datetime_fromisoformat | jit | 0.3924 | 0.4513 | 0.4486 | 0.9845 | 0.3924 |
| datetime_fromisoformat | interp | 0.4042 | 0.4616 | 0.4585 | 0.9918 | 0.4042 |
| date_construct | jit | 1.0270 | 1.0162 | 1.0178 | 0.9990 | 1.0272 |
| date_construct | interp | 1.0154 | 1.0137 | 1.0142 | 0.9989 | 1.0155 |
| date_arithmetic | jit | 1.0074 | 1.0086 | 1.0084 | 1.0015 | 1.0075 |
| date_arithmetic | interp | 1.0099 | 1.0105 | 1.0110 | 1.0022 | 1.0091 |

### components-isolated: ratios against CPython

| Probe | Mode | Work time | Process time | Process CPU | Peak RSS | Work CPU |
|---|---|---:|---:|---:|---:|---:|
| datetime_construct | jit | 53.1495 | 3.6310 | 3.8768 | 2.2672 | 53.3059 |
| datetime_construct | interp | 52.8483 | 3.4936 | 3.7319 | 1.8871 | 53.0086 |
| timedelta_construct | jit | 7.5634 | 2.0778 | 2.1697 | 2.1655 | 7.5744 |
| timedelta_construct | interp | 7.3992 | 1.9786 | 2.0734 | 1.8906 | 7.4067 |
| datetime_add | jit | 933.7353 | 10.2390 | 11.0552 | 2.2944 | 933.7059 |
| datetime_add | interp | 895.5613 | 9.8006 | 10.5880 | 1.8933 | 895.5294 |
| datetime_subtract | jit | 146.3680 | 2.9846 | 3.1798 | 2.1719 | 149.1707 |
| datetime_subtract | interp | 138.2782 | 2.7996 | 2.9687 | 1.8898 | 140.9390 |
| timedelta_multiply | jit | 21.7296 | 2.3395 | 2.4780 | 2.0523 | 21.8067 |
| timedelta_multiply | interp | 22.0192 | 2.2569 | 2.3787 | 1.8873 | 22.0982 |
| datetime_fields | jit | 29.0584 | 2.5052 | 2.6294 | 2.0709 | 29.1845 |
| datetime_fields | interp | 21.4767 | 2.1183 | 2.2211 | 1.8840 | 21.5641 |
| datetime_weekday | jit | 63.5180 | 2.1021 | 2.2175 | 2.0641 | 64.8529 |
| datetime_weekday | interp | 35.5533 | 1.8044 | 1.8916 | 1.8939 | 36.2794 |
| datetime_isoformat | jit | 64.5989 | 8.7173 | 9.4491 | 2.0649 | 64.6552 |
| datetime_isoformat | interp | 64.5821 | 8.7091 | 9.3641 | 1.8953 | 64.6389 |
| datetime_strftime | jit | 14.3186 | 4.6777 | 4.9624 | 2.0531 | 14.3219 |
| datetime_strftime | interp | 12.8315 | 4.2364 | 4.4627 | 1.8884 | 12.8382 |
| datetime_fromisoformat | jit | 291.3852 | 7.5164 | 8.1755 | 2.2651 | 293.9471 |
| datetime_fromisoformat | interp | 270.4768 | 6.9023 | 7.5025 | 1.8928 | 272.8519 |
| date_construct | jit | 63.3186 | 2.5919 | 2.7534 | 2.1280 | 63.4257 |
| date_construct | interp | 60.5701 | 2.4816 | 2.6389 | 1.8940 | 60.9800 |
| date_arithmetic | jit | 85.5008 | 4.5087 | 4.8521 | 2.1649 | 85.5455 |
| date_arithmetic | interp | 84.5798 | 4.3696 | 4.6923 | 1.8944 | 84.6303 |

### controls-isolated: ratios against e910f2dd

| Probe | Mode | Work time | Process time | Process CPU | Peak RSS | Work CPU |
|---|---|---:|---:|---:|---:|---:|
| time_hours | jit | 0.8587 | 0.9057 | 0.9040 | 1.0024 | 0.8587 |
| time_hours | interp | 0.8647 | 0.9166 | 0.9163 | 1.0000 | 0.8647 |
| time_minutes | jit | 0.7774 | 0.8600 | 0.8574 | 1.0024 | 0.7774 |
| time_minutes | interp | 0.7788 | 0.8612 | 0.8589 | 0.9994 | 0.7786 |
| time_seconds | jit | 0.7328 | 0.8284 | 0.8242 | 1.0014 | 0.7328 |
| time_seconds | interp | 0.7335 | 0.8240 | 0.8212 | 0.9994 | 0.7333 |
| fraction_1 | jit | 1.0166 | 1.0145 | 1.0149 | 1.0005 | 1.0166 |
| fraction_1 | interp | 1.0066 | 1.0065 | 1.0070 | 0.9956 | 1.0066 |
| fraction_5 | jit | 1.0159 | 1.0139 | 1.0140 | 1.0019 | 1.0159 |
| fraction_5 | interp | 1.0167 | 1.0190 | 1.0193 | 0.9962 | 1.0167 |
| fraction_6 | jit | 0.1994 | 0.3022 | 0.2972 | 0.9853 | 0.1994 |
| fraction_6 | interp | 0.2124 | 0.3204 | 0.3153 | 0.9901 | 0.2124 |
| fraction_9 | jit | 0.1781 | 0.2723 | 0.2680 | 0.9844 | 0.1780 |
| fraction_9 | interp | 0.1894 | 0.2904 | 0.2852 | 0.9913 | 0.1894 |
| fraction_comma | jit | 0.1977 | 0.3003 | 0.2954 | 0.9848 | 0.1976 |
| fraction_comma | interp | 0.2133 | 0.3222 | 0.3171 | 0.9907 | 0.2133 |
| utc | jit | 0.2163 | 0.3130 | 0.3084 | 0.9848 | 0.2162 |
| utc | interp | 0.2295 | 0.3337 | 0.3282 | 0.9880 | 0.2295 |
| offset_fraction | jit | 0.2591 | 0.3112 | 0.3090 | 0.9880 | 0.2591 |
| offset_fraction | interp | 0.3089 | 0.3568 | 0.3546 | 0.9885 | 0.3089 |
| unicode_separator | jit | 0.3195 | 0.3956 | 0.3922 | 0.9846 | 0.3195 |
| unicode_separator | interp | 0.3381 | 0.4133 | 0.4096 | 0.9934 | 0.3381 |
| str_subclass | jit | 0.2021 | 0.3038 | 0.2980 | 0.9844 | 0.2020 |
| str_subclass | interp | 0.2149 | 0.3221 | 0.3163 | 0.9885 | 0.2149 |
| incomplete_component | jit | 1.0103 | 1.0021 | 1.0028 | 1.0005 | 1.0103 |
| incomplete_component | interp | 1.0194 | 1.0125 | 1.0128 | 1.0006 | 1.0194 |
| invalid_range | jit | 0.8751 | 0.8947 | 0.8937 | 0.9899 | 0.8750 |
| invalid_range | interp | 0.8708 | 0.8930 | 0.8916 | 0.9858 | 0.8702 |
| invalid_fraction | jit | 1.0091 | 1.0087 | 1.0089 | 1.0010 | 1.0091 |
| invalid_fraction | interp | 0.9903 | 1.0006 | 1.0016 | 0.9940 | 0.9903 |

### controls-isolated: ratios against CPython

| Probe | Mode | Work time | Process time | Process CPU | Peak RSS | Work CPU |
|---|---|---:|---:|---:|---:|---:|
| time_hours | jit | 145.9008 | 3.7540 | 4.0288 | 2.1786 | 147.5000 |
| time_hours | interp | 139.0159 | 3.5302 | 3.7950 | 1.8960 | 140.2296 |
| time_minutes | jit | 144.7562 | 3.7622 | 3.9983 | 2.1805 | 145.9627 |
| time_minutes | interp | 136.8356 | 3.5122 | 3.7603 | 1.9012 | 137.8074 |
| time_seconds | jit | 135.0677 | 3.7217 | 3.9821 | 2.1749 | 136.2721 |
| time_seconds | interp | 128.0953 | 3.5168 | 3.7708 | 1.8953 | 129.0541 |
| fraction_1 | jit | 562.6990 | 10.5795 | 11.4757 | 2.2086 | 564.9178 |
| fraction_1 | interp | 490.6932 | 9.2845 | 10.0656 | 1.9099 | 497.3537 |
| fraction_5 | jit | 627.1435 | 12.1397 | 13.2365 | 2.2096 | 631.9490 |
| fraction_5 | interp | 546.2890 | 10.7726 | 11.7585 | 1.9086 | 550.4777 |
| fraction_6 | jit | 119.5888 | 3.6988 | 3.9767 | 2.1723 | 120.1852 |
| fraction_6 | interp | 114.8536 | 3.4831 | 3.7395 | 1.8966 | 115.4321 |
| fraction_9 | jit | 125.3047 | 3.7661 | 4.0409 | 2.1715 | 126.2949 |
| fraction_9 | interp | 118.0018 | 3.5390 | 3.7936 | 1.9000 | 117.7107 |
| fraction_comma | jit | 128.4729 | 3.6875 | 3.9584 | 2.1726 | 129.3421 |
| fraction_comma | interp | 120.1791 | 3.4850 | 3.7410 | 1.8987 | 121.1908 |
| utc | jit | 136.4899 | 3.9474 | 4.2358 | 2.1677 | 137.1572 |
| utc | interp | 129.8558 | 3.7247 | 3.9995 | 1.8945 | 130.3671 |
| offset_fraction | jit | 292.0344 | 9.1454 | 9.9331 | 2.3979 | 293.9221 |
| offset_fraction | interp | 268.7464 | 8.2761 | 8.9896 | 1.8899 | 270.1760 |
| unicode_separator | jit | 213.5541 | 5.6924 | 6.1438 | 2.2725 | 214.3295 |
| unicode_separator | interp | 205.5421 | 5.3646 | 5.7881 | 1.8956 | 206.7955 |
| str_subclass | jit | 130.5287 | 3.7284 | 4.0185 | 2.1761 | 130.5519 |
| str_subclass | interp | 120.3754 | 3.5259 | 3.8025 | 1.8963 | 121.2710 |
| incomplete_component | jit | 87.0271 | 6.4223 | 6.9492 | 2.0546 | 87.1537 |
| incomplete_component | interp | 82.6755 | 6.1123 | 6.6107 | 1.8873 | 82.7289 |
| invalid_range | jit | 93.6925 | 6.8900 | 7.4281 | 2.0587 | 93.8067 |
| invalid_range | interp | 89.4978 | 6.5584 | 7.0683 | 1.8960 | 89.6990 |
| invalid_fraction | jit | 196.8531 | 13.1210 | 14.1886 | 2.0912 | 197.2240 |
| invalid_fraction | interp | 184.3090 | 12.1967 | 13.1713 | 1.9057 | 184.6119 |

## Full census with separate frozen caches

Five paired samples compare checkpoint 9a69c41, preceding complete release e910f2dd, and CPython. All 24 cache-stability checks pass. Geometric means include 24 workloads for process metrics and exclude startup only from the 23-workload isolated timer aggregate.

| Metric | JIT/checkpoint | Interpreter/checkpoint | JIT/e910 | Interpreter/e910 | JIT/CPython | Interpreter/CPython |
|---|---:|---:|---:|---:|---:|---:|
| Workload time, 23 workloads | 0.7957 | 0.9471 | 0.9960 | 1.0112 | 3.4400 | 9.4436 |
| Process time, 24 workloads | 0.8552 | 0.9545 | 0.9990 | 1.0101 | 3.2600 | 5.7837 |
| CPU time, 24 workloads | 0.8535 | 0.9545 | 0.9992 | 1.0100 | 3.3330 | 5.9768 |
| Peak RSS, 24 workloads | 0.9420 | 0.9345 | 0.9999 | 0.9972 | 2.0734 | 1.9047 |

WeavePy wins 6/23 JIT workload timers and 0/24 JIT peak-RSS comparisons. The overall objective remains unachieved.

| Workload | JIT time/e910 | Interpreter time/e910 | JIT RSS/e910 | Interpreter RSS/e910 | JIT time/CPython | JIT RSS/CPython |
|---|---:|---:|---:|---:|---:|---:|
| fannkuch | 0.9893 | 1.0045 | 0.9951 | 1.0000 | 8.9624 | 1.9676 |
| nbody | 1.0180 | 1.0095 | 0.9962 | 0.9971 | 8.9091 | 1.9640 |
| fib | 0.9976 | 1.0121 | 0.9946 | 0.9982 | 2.9738 | 1.9827 |
| pidigits | 1.0021 | 0.9996 | 1.0037 | 0.9977 | 0.8909 | 1.9657 |
| pyaes | 0.9982 | 1.0006 | 1.0000 | 0.9923 | 0.6708 | 1.9692 |
| richards | 1.0036 | 1.0050 | 1.0016 | 0.9947 | 8.3931 | 1.9762 |
| sumvm | 1.0133 | 1.0276 | 0.9995 | 0.9965 | 0.0565 | 1.9827 |
| nested_loops | 0.8560 | 1.0199 | 1.0022 | 0.9959 | 0.0818 | 1.9859 |
| jitloop | 0.9905 | 1.0226 | 1.0005 | 0.9953 | 0.0725 | 1.9903 |
| jitkernels | 1.0040 | 1.0121 | 1.0011 | 0.9977 | 0.8901 | 1.9807 |
| deltablue | 1.0008 | 1.0097 | 1.0000 | 1.0000 | 19.5002 | 2.1486 |
| float_math | 1.0068 | 1.0047 | 0.9995 | 0.9994 | 7.2792 | 2.9887 |
| spectral_norm | 1.0020 | 1.0098 | 1.0011 | 0.9977 | 2.1111 | 1.9893 |
| json_bench | 1.0023 | 0.9962 | 1.0025 | 1.0004 | 1.1411 | 2.4828 |
| str_methods | 0.9951 | 1.0413 | 0.9970 | 0.9906 | 2.0076 | 2.1670 |
| dict_ops | 1.0069 | 1.0075 | 0.9962 | 0.9982 | 5.5380 | 1.9519 |
| list_ops | 1.0046 | 1.0119 | 0.9978 | 1.0000 | 13.3948 | 1.9666 |
| attr_access | 1.0123 | 1.0007 | 1.0016 | 0.9982 | 2.6931 | 2.0204 |
| call_overhead | 0.9981 | 1.0090 | 1.0016 | 0.9959 | 8.8060 | 2.0278 |
| generators | 1.0029 | 1.0236 | 1.0005 | 0.9977 | 9.5569 | 1.9860 |
| deque_ops | 1.0090 | 1.0093 | 1.0010 | 0.9960 | 16.8955 | 2.0055 |
| datetime_ops | 0.9987 | 1.0212 | 1.0020 | 0.9978 | 152.7767 | 2.1263 |
| pickle_bench | 1.0073 | 0.9999 | 0.9996 | 0.9986 | 207.2389 | 2.4287 |
| startup | 1.0006 | 1.0012 | 1.0022 | 0.9959 | 1.4163 | 1.9697 |

## Controlled startup

Thirty-one paired samples use JIT-disabled execution, equal-length executable paths, isolated caches, and verified matching or relocated serialized code filenames. Frozen artifacts stay unchanged.

| Case | Elapsed/e910 | CPU/e910 | RSS/e910 | Elapsed/CPython | CPU/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|---:|
| startup_matched | 1.0038 | 1.0037 | 0.9994 | 1.3824 | 1.4354 | 1.8332 |
| startup_relocated | 0.9987 | 0.9995 | 0.9976 | 1.3715 | 1.4300 | 1.8359 |
| no_site_matched | 1.0325 | 1.0273 | 0.9983 | 0.6171 | 0.5914 | 1.5545 |
| imports_matched | 0.9982 | 0.9978 | 0.9996 | 2.8012 | 2.9724 | 2.4307 |
| imports_relocated | 0.9998 | 1.0009 | 1.0000 | 2.8086 | 2.9766 | 2.4316 |
| pickle_import_matched | 0.9969 | 0.9976 | 0.9981 | 2.2151 | 2.3507 | 2.2300 |
| pickle_import_relocated | 0.9988 | 1.0003 | 0.9972 | 2.2253 | 2.3745 | 2.2303 |
| accelerator_first_matched | 1.0040 | 1.0059 | 0.9958 | 2.1986 | 2.3442 | 2.2240 |
| accelerator_first_relocated | 1.0009 | 1.0004 | 0.9958 | 2.2139 | 2.3456 | 2.2269 |

## Limits

All new and remaining regressions stay part of the assessment. The component wins do not establish overall CPython superiority. Energy, controlled build latency, parallel throughput, GC-pause distributions, and every Python workload were not measured. GIL-disabled correctness does not establish free-threaded performance.
