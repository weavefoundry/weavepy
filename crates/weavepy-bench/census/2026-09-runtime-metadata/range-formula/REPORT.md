# Exact range sums and bounded iterator advancement

Exact ranges with exact integer or boolean starts now use the arithmetic-series formula. Ordinary bounds use checked i128 arithmetic, dividing an even factor before multiplication; wide values use full-precision integers and the real range bounds. Empty integer starts retain their original objects. Noninteger starts, custom iterators, and generators retain per-element arithmetic and callbacks. The VM and static builtin share the helper. No new unsafe code, public Rust API, or object layout is introduced.

Boundary probes exposed a preexisting infinite loop: the final advance of a valid i128 range could overflow and return the cursor to its starting value. Checked advancement now exhausts at the stop when the next value crosses that limit. Snapshots use the same checked boundary, wide remaining counts use full precision, and i64 spans widen before subtraction. Range length hints return full Python integers instead of wrapping or reporting zero for large counts.

Candidate SHA-256: `337990883d962526498cd2f6c88d9de676babed1b7f4d0c347b5c55b60bbf789`; 44,211,904 bytes, 112 bytes above the numeric-sum release. All 12 measured layouts are unchanged. Frozen inputs contain 121 sources and 37 measurement inputs.

All 321 VM tests, 52 JIT tests, 265 configured compatibility checks, Clippy, and workspace/all-feature/no-JIT checks pass. The focused range filter passes 13 tests. Two new permanent fixtures cover 283 sum expressions plus empty-start identity checks, and 40 wide-iteration cases plus three huge hints. Five first-call/warmed benchmark return checks match CPython in native, interpreted, and GIL-disabled modes.

The isolated 284-case sum oracle now matches CPython on 283 cases in all three VM modes, repairing the timeout. One existing empty-float identity difference remains unchanged: CPython creates a different float result, while WeavePy retains inline float identity. Exact integer, boolean, and custom empty starts agree. All 40 forward-iteration cases and all three huge hints now match CPython, repairing 26 differing iteration probes and three hint probes. Each case runs with a two-second subprocess deadline, so original hangs remain bounded and recorded.

The expanded 764-case numeric oracle retains exactly its 46 known differences without new differences. The original 47-case sum oracle retains only three existing TypeError wording differences. These checks do not establish complete range or numeric compatibility. Adjacent wide-range reversal, equality, index/count, and live iterator state handling have additional unchecked arithmetic or representation limitations identified during review; this stage repairs forward advancement, hints, and exact sums.

Focused groups use seven alternating paired cycles after a discarded cycle, comparing against the f655 numeric-sum release. Every run takes place outside the tool filesystem sandbox and records its caller-declared launch context and timezone/locale/Python environment. Cold controls omit explicit workload warmup; operating-system caches are not flushed. Every gain, regression, and raw sample is retained.

The 10,000-element zero-start range timer takes 0.130 percent of the preceding release and 1.03 percent of CPython time. The two-million-element case takes 0.0102 percent of the preceding release and 0.0793 percent of CPython time; its complete process takes about 7.8 percent less elapsed time than CPython, with about 84 percent higher peak RSS. Two-million-element boolean and large-integer starts take about 47 and 51 percent less process elapsed time than CPython. Empty, one-element, and 32-element range controls remain about 5.2, 5.4, and 3.0 times slower than CPython in their repeated-reduction timers. Float-start range sums remain about 7.85 times CPython workload time.

Unrelated controls retain both outcomes. Attribute time improves about 1.4 percent cold and 1.2 percent warm, while list operations regress about 4.4 and 4.3 percent. The two-million-integer list sum timer regresses about 3.5 percent in the sum group, while the population construction timer improves about 3.5 percent. The large-list population still uses about 20 percent less peak RSS than CPython; tuples remain about 9 percent higher. All samples remain available.

Sum timers exclude construction; population timers measure construction but exclude their later checksum. Process elapsed time, CPU, and peak RSS include startup and validation. Custom probes use interpreter mode. Small-range probes repeat reductions to make call/loop overhead visible; the table reports the number of reductions per sample. One wide-i128 timing case is explicitly excluded because the preceding release hangs; its before/after correctness results remain in the archive. No enormous CPython range is iterated.

## cold

| Fixture | Reductions | Time/previous | Wall/previous | CPU/previous | RSS/previous | Time/CPython | Wall/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| str_methods |  | 1.000617 | 0.991218 | 0.999374 | 1.000495 | 2.026414 | 1.807623 | 2.174194 |
| list_ops |  | 1.043897 | 1.037870 | 1.037895 | 0.992408 | 14.262822 | 8.189339 | 1.964516 |
| attr_access |  | 0.986426 | 0.983678 | 0.984489 | 0.998407 | 3.143961 | 2.467366 | 2.022484 |
| call_overhead |  | 1.013988 | 1.012203 | 1.012395 | 0.995253 | 9.181380 | 6.746298 | 2.029979 |
| jitkernels |  | 1.003768 | 0.986087 | 0.985230 | 0.998921 | 0.878044 | 1.099805 | 1.975584 |

## warm

| Fixture | Reductions | Time/previous | Wall/previous | CPU/previous | RSS/previous | Time/CPython | Wall/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| str_methods |  | 0.996767 | 0.987638 | 0.989812 | 1.000487 | 1.938292 | 1.834139 | 2.146138 |
| list_ops |  | 1.042967 | 1.027716 | 1.008233 | 1.001616 | 14.477433 | 10.124978 | 1.946597 |
| attr_access |  | 0.988140 | 0.987648 | 0.989483 | 1.001571 | 3.141204 | 2.697685 | 1.991701 |
| call_overhead |  | 0.997714 | 1.001169 | 1.000762 | 1.000000 | 8.890170 | 7.417770 | 1.993769 |
| jitkernels |  | 0.998693 | 0.998784 | 1.000337 | 1.000532 | 0.836708 | 0.999557 | 1.947150 |

## sums

| Fixture | Reductions | Time/previous | Wall/previous | CPU/previous | RSS/previous | Time/CPython | Wall/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| list_ints_32 |  | 1.021739 | 1.000222 | 1.001101 | 0.997651 | 1.322581 | 1.380790 | 1.845238 |
| list_ints_10000 |  | 0.988632 | 1.001072 | 1.000537 | 0.998235 | 0.361746 | 1.390801 | 1.794709 |
| list_ints_1000000 |  | 0.985936 | 0.976845 | 0.993124 | 0.998739 | 0.275014 | 0.804837 | 0.939205 |
| list_ints_2000000 |  | 1.034938 | 1.003339 | 0.996113 | 0.998922 | 0.298727 | 0.610983 | 0.795810 |
| list_bools |  | 1.009665 | 0.991551 | 1.006941 | 0.995215 | 0.324679 | 1.410347 | 2.050179 |
| list_overflow |  | 1.011905 | 0.993401 | 0.994098 | 1.003990 | 0.048236 | 1.323010 | 1.975394 |
| list_floats |  | 1.022575 | 0.996143 | 1.000740 | 1.001492 | 0.575102 | 1.386173 | 1.979269 |
| list_mixed |  | 1.022300 | 0.984620 | 0.998816 | 1.000000 | 0.780507 | 1.402742 | 1.970443 |
| list_big_last |  | 0.995513 | 0.979300 | 0.996164 | 1.000498 | 0.760437 | 1.418996 | 1.973399 |
| tuple_ints_32 |  | 1.034051 | 0.986720 | 0.989911 | 0.999411 | 1.250000 | 1.406194 | 1.844086 |
| tuple_ints_10000 |  | 1.031218 | 0.978918 | 0.985733 | 0.997091 | 0.400286 | 1.379178 | 1.805058 |
| tuple_ints_1000000 |  | 0.955222 | 0.984947 | 0.986765 | 0.999785 | 0.330211 | 0.881072 | 1.165495 |
| tuple_ints_2000000 |  | 0.978547 | 0.989371 | 0.993916 | 0.999471 | 0.335226 | 0.709465 | 1.093023 |
| tuple_bools |  | 1.036154 | 0.980182 | 0.973920 | 0.997680 | 0.330445 | 1.473842 | 2.314798 |
| tuple_overflow |  | 0.958761 | 0.986507 | 0.985793 | 0.999534 | 0.047090 | 1.328381 | 2.120673 |
| tuple_floats |  | 0.982862 | 0.991138 | 0.990572 | 1.001393 | 0.618338 | 1.414588 | 2.114511 |
| tuple_mixed |  | 1.008260 | 0.994297 | 0.997519 | 0.999536 | 0.766015 | 1.411470 | 2.117011 |
| tuple_big_last |  | 0.998295 | 0.987518 | 0.987621 | 1.000464 | 0.769060 | 1.410581 | 2.119094 |
| range_2000000 |  | 0.000072 | 0.255836 | 0.242546 | 0.998240 | 0.000567 | 0.910993 | 1.843987 |
| generator_100000 |  | 1.005201 | 0.993667 | 0.994429 | 1.000000 | 10.344059 | 2.038132 | 1.849077 |

## populations

| Fixture | Reductions | Time/previous | Wall/previous | CPU/previous | RSS/previous | Time/CPython | Wall/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| list_range_10000 |  | 0.964454 | 0.976725 | 0.980545 | 0.996475 | 0.228029 | 1.404754 | 1.794521 |
| list_range_100000 |  | 1.081145 | 0.965903 | 0.970634 | 0.997302 | 0.364920 | 1.307022 | 1.591731 |
| list_range_500000 |  | 1.011868 | 0.984899 | 0.986781 | 1.000409 | 0.322714 | 1.012015 | 1.135538 |
| list_range_1000000 |  | 1.023481 | 0.993884 | 0.988940 | 0.999054 | 0.326723 | 0.804294 | 0.940059 |
| list_range_2000000 |  | 0.965171 | 0.983130 | 0.990850 | 0.999352 | 0.321930 | 0.605355 | 0.795673 |
| tuple_range_10000 |  | 1.096048 | 0.990023 | 0.988143 | 0.997095 | 0.472414 | 1.387649 | 1.802105 |
| tuple_range_100000 |  | 0.992255 | 0.970954 | 0.986632 | 0.996012 | 0.595848 | 1.306413 | 1.645507 |
| tuple_range_500000 |  | 1.033058 | 0.988048 | 0.985987 | 1.000315 | 0.601670 | 1.049144 | 1.259436 |
| tuple_range_1000000 |  | 1.033254 | 0.986242 | 0.990014 | 0.999784 | 0.605446 | 0.866413 | 1.163071 |
| tuple_range_2000000 |  | 0.989108 | 0.983944 | 0.987269 | 0.999472 | 0.597964 | 0.714699 | 1.093023 |
| list_repeated_10000 |  | 1.006110 | 0.986753 | 0.988679 | 0.996497 | 2.595200 | 1.378532 | 1.835853 |
| list_repeated_100000 |  | 1.048479 | 0.990022 | 0.990658 | 1.000539 | 5.114490 | 1.371096 | 1.918050 |
| list_repeated_500000 |  | 0.993190 | 0.991976 | 0.996263 | 1.000000 | 4.451829 | 1.340232 | 2.085616 |
| list_repeated_1000000 |  | 1.013800 | 0.984913 | 0.985725 | 0.997167 | 4.671968 | 1.304375 | 2.243281 |
| list_repeated_2000000 |  | 0.970835 | 0.989629 | 0.993958 | 0.999569 | 4.605076 | 1.259773 | 2.439769 |
| tuple_repeated_10000 |  | 0.993833 | 0.999880 | 0.995820 | 0.999419 | 5.114494 | 1.403641 | 1.851613 |
| tuple_repeated_100000 |  | 1.024817 | 1.002586 | 1.005819 | 0.996505 | 8.808539 | 1.404673 | 2.069430 |
| tuple_repeated_500000 |  | 0.965973 | 0.981341 | 0.980817 | 0.997485 | 7.848941 | 1.375298 | 2.715753 |
| tuple_repeated_1000000 |  | 0.999329 | 0.997163 | 0.990102 | 1.000000 | 9.027743 | 1.422994 | 3.280057 |
| tuple_repeated_2000000 |  | 0.972157 | 0.986292 | 0.992204 | 0.999736 | 8.344727 | 1.442151 | 3.980000 |

## ranges

| Fixture | Reductions | Time/previous | Wall/previous | CPU/previous | RSS/previous | Time/CPython | Wall/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| range_0_zero | 20000 | 0.906836 | 0.985375 | 0.990438 | 0.997071 | 5.223850 | 1.679351 | 1.839093 |
| range_0_bool | 20000 | 0.913788 | 0.964013 | 0.965284 | 0.999413 | 5.741402 | 1.705892 | 1.849241 |
| range_0_big | 20000 | 1.003420 | 0.992568 | 0.998427 | 0.998244 | 6.102055 | 1.718007 | 1.844492 |
| range_0_float | 20000 | 1.028875 | 0.991299 | 0.996978 | 1.002929 | 5.844371 | 1.710585 | 1.848812 |
| range_1_zero | 20000 | 0.826261 | 0.946347 | 0.951824 | 0.998239 | 5.366666 | 1.700583 | 1.837838 |
| range_1_bool | 20000 | 0.836251 | 0.944010 | 0.945073 | 1.000000 | 5.097784 | 1.719060 | 1.844156 |
| range_1_big | 20000 | 1.206943 | 1.057187 | 1.055659 | 0.999413 | 7.360888 | 1.927463 | 1.844156 |
| range_1_float | 20000 | 1.022297 | 0.996330 | 1.001904 | 0.998240 | 6.383521 | 1.790775 | 1.843243 |
| range_32_zero | 10000 | 0.251767 | 0.704943 | 0.690073 | 0.999412 | 3.013658 | 1.515406 | 1.838919 |
| range_32_bool | 10000 | 0.248646 | 0.699939 | 0.688928 | 0.999413 | 1.560653 | 1.401968 | 1.842904 |
| range_32_big | 10000 | 0.145268 | 0.440594 | 0.426675 | 0.998831 | 1.360457 | 1.374889 | 1.841820 |
| range_32_float | 10000 | 0.987291 | 0.982734 | 0.986921 | 0.998828 | 12.514270 | 2.301693 | 1.841081 |
| range_10000_zero | 100 | 0.001301 | 0.406826 | 0.388807 | 0.996479 | 0.010345 | 1.111270 | 1.838570 |
| range_10000_bool | 100 | 0.001282 | 0.405399 | 0.389885 | 0.996481 | 0.004614 | 0.881313 | 1.841820 |
| range_10000_big | 100 | 0.000587 | 0.162278 | 0.152622 | 0.998834 | 0.004608 | 0.720968 | 1.850325 |
| range_10000_float | 100 | 0.995425 | 0.991131 | 0.992907 | 0.998238 | 7.943853 | 3.094278 | 1.841991 |
| range_2000000_zero | 1 | 0.000102 | 0.262094 | 0.247760 | 0.996491 | 0.000793 | 0.922472 | 1.843818 |
| range_2000000_bool | 1 | 0.000096 | 0.259615 | 0.246312 | 0.992978 | 0.000231 | 0.531729 | 1.835853 |
| range_2000000_big | 1 | 0.000031 | 0.087200 | 0.081538 | 0.998827 | 0.000245 | 0.490659 | 1.847403 |
| range_2000000_float | 1 | 0.996866 | 0.989484 | 0.989075 | 0.997073 | 7.853603 | 3.969377 | 1.846320 |
| descending | 1 | 0.000184 | 0.408423 | 0.387908 | 0.999416 | 0.001441 | 1.112144 | 1.849404 |
| wide_big | 1000 | 0.126959 | 0.807703 | 0.799490 | 1.001172 | 0.600386 | 1.343732 | 1.851410 |

## Full census

The 24-fixture refresh uses five alternating paired cycles after a discarded cycle. Its base is checkpoint 9a69c41; its previous reference is the 7ef09 streaming-sum release, the last full census. This differs from the focused preceding release. Aggregates are geometric means of per-fixture median paired ratios. Workload time excludes startup, leaving 23 timers; process elapsed time, CPU, and peak RSS include all 24.

| Metric | JIT/checkpoint | Interpreter/checkpoint | JIT/streaming sum | Interpreter/streaming sum | JIT/CPython | Interpreter/CPython |
|---|---:|---:|---:|---:|---:|---:|
| ns | 0.8276 | 0.9684 | 1.0033 | 1.0089 | 3.5637 | 9.6947 |
| wall_ns | 0.8903 | 0.9796 | 0.9941 | 1.0114 | 3.3896 | 5.9098 |
| cpu_ns | 0.8896 | 0.9793 | 0.9960 | 1.0113 | 3.4712 | 6.1214 |
| rss_bytes | 0.9408 | 0.9346 | 0.9860 | 0.9911 | 2.0799 | 1.9153 |

Against the preceding complete streaming-sum census, aggregate JIT workload time rises about 0.3 percent, process elapsed time falls about 0.6 percent, CPU time falls about 0.4 percent, and peak RSS falls about 1.4 percent. Interpreter workload time rises about 0.9 percent and peak RSS falls about 0.9 percent. The focused reduction gains do not translate into superiority across the standard suite.

JIT workload-time wins against CPython: 6/23. Peak-RSS wins: 0/24. The objective of superiority across every meaningful metric remains unachieved.

| Fixture | Time/streaming sum | CPU/streaming sum | RSS/streaming sum | Time/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|---:|
| fannkuch | 1.0117 | 1.0075 | 0.9913 | 9.0295 | 1.9645 |
| nbody | 1.0164 | 1.0116 | 0.9920 | 8.9925 | 1.9662 |
| fib | 1.0009 | 0.9997 | 0.9909 | 3.0369 | 1.9892 |
| pidigits | 1.0044 | 1.0044 | 0.9870 | 0.8947 | 1.9740 |
| pyaes | 1.0015 | 0.9892 | 0.9894 | 0.6614 | 1.9672 |
| richards | 1.0010 | 0.9961 | 0.9903 | 8.3820 | 1.9764 |
| sumvm | 1.0074 | 0.9815 | 0.9908 | 0.0568 | 1.9827 |
| nested_loops | 0.9969 | 0.9812 | 0.9909 | 0.0801 | 1.9946 |
| jitloop | 1.0003 | 0.9834 | 0.9903 | 0.0732 | 1.9935 |
| jitkernels | 1.0108 | 0.9899 | 0.9893 | 0.8958 | 1.9702 |
| deltablue | 0.9883 | 0.9895 | 0.9933 | 19.6260 | 2.1550 |
| float_math | 1.0130 | 1.0120 | 0.9976 | 7.6197 | 2.9896 |
| spectral_norm | 1.0073 | 0.9953 | 0.9888 | 2.1568 | 1.9925 |
| json_bench | 0.9994 | 0.9800 | 0.9352 | 1.1478 | 2.5477 |
| str_methods | 1.0008 | 0.9925 | 0.9911 | 2.0068 | 2.1710 |
| dict_ops | 1.0099 | 1.0079 | 0.9897 | 5.5488 | 1.9624 |
| list_ops | 1.0044 | 1.0034 | 0.9913 | 13.7030 | 1.9591 |
| attr_access | 1.0000 | 0.9954 | 0.9926 | 3.1251 | 2.0258 |
| call_overhead | 1.0011 | 1.0000 | 0.9874 | 9.1708 | 2.0333 |
| generators | 1.0036 | 0.9993 | 0.9888 | 9.5229 | 2.0011 |
| deque_ops | 1.0046 | 1.0040 | 0.9929 | 16.8599 | 2.0180 |
| datetime_ops | 0.9940 | 0.9939 | 0.9896 | 154.3177 | 2.1380 |
| pickle_bench | 0.9999 | 0.9971 | 0.9350 | 337.4999 | 2.4513 |
| startup | 0.9862 | 0.9893 | 0.9918 | 1.4922 | 1.9718 |

Actual OS peak RSS determines memory comparisons. The illustrative fannkuch fixture is not canonical fannkuch, and pyaes is an XOR scrambler. The preceding two full censuses had different sandbox conditions; controlled probes in the streaming-sum archive explain the resulting CPython datetime shift. This refresh consistently uses the outside-sandbox condition.

Dedicated startup/import variants, parallel throughput, and explicit GC-pause distributions are not repeated. CPython is the recorded 3.14.7 GIL build, and WeavePy native execution remains gated with the GIL disabled. No free-threaded CPython, energy, or controlled build-latency claim is made.
