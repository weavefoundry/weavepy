# Cached-code buffer compaction

This candidate shrinks 13 decoded code vectors after taking ownership of the root and while relocating nested filenames. The existing copy-on-write behavior for shared code is preserved. Matching filenames only compact the root; nested compaction follows the existing relocation walk. No bytecode-format, object-layout, public API, or unsafe-code change is introduced.

Candidate SHA-256: `63ed378aa7d6eeeadd43f7bc6e1867ae4b6548d7deb5e4c2ca750061ea301856`. Executable size: 44,212,096 bytes, 192 bytes below the ownership release and 192 bytes above the range-formula release. The snapshot records 124 source files and 37 measurement inputs. All 12 measured layouts are unchanged.

All 323 VM tests, 52 JIT tests, Clippy, workspace/all-feature/no-JIT checks, and 269 positive compatibility checks pass. Both ownership tests now exercise the actual loader helper; the decoded case also compares all code metadata. Exact marshal/import/importlib diagnostics retain the preceding release's outcomes: test_import.py passes, and the configured marshal divergence and importlib failure are unchanged. Complete raw results are retained. Five independent workload return checks match CPython on both first and warmed calls in JIT, interpreted, and GIL-disabled parent modes. Native execution, builder, and scalar-leaf proofs pass. These checks do not establish full CPython compatibility.

The candidate has a measured tradeoff. Aggregate JIT peak RSS falls about 0.8 percent versus ownership, while JIT workload time rises about 0.6 percent. Interpreter workload time rises about 1.3 percent and its peak RSS is essentially unchanged. Relocated-import peak RSS falls about 2.25 percent versus ownership, but remains about 1.04 percent above the range-formula release. Several focused list-operation and interpreted string controls regress. The compaction approach needs further investigation; these results do not meet the overall objective.

All timing uses the declared outside-tool-filesystem-sandbox condition. Standard controls and the full census retain their historical shared metadata-cache arrangement; their workload timers exclude process startup. Supplemental startup probes use equal-length executable paths, separate stdlib metadata caches and frozen-code caches, verify serialized and runtime filenames, and verify frozen artifacts remain unchanged. OS caches are not flushed, so these are warm-cache starts, not first extraction.

## Cold and warm controls

Seven alternating paired cycles follow a discarded cycle. Previous means the ownership release; all ratios below one indicate a reduction.

| Condition | Fixture | JIT time/previous | Interpreted time/previous | JIT RSS/previous | JIT time/CPython | JIT RSS/CPython |
|---|---|---:|---:|---:|---:|---:|
| cold | str_methods | 1.007333 | 1.050207 | 0.992683 | 2.017624 | 2.197624 |
| cold | list_ops | 1.065743 | 1.056888 | 0.998381 | 14.577707 | 1.986022 |
| cold | attr_access | 1.000481 | 1.003106 | 0.996325 | 3.116438 | 2.045356 |
| cold | call_overhead | 1.007726 | 1.009609 | 0.996875 | 9.136159 | 2.054957 |
| cold | jitkernels | 0.996295 | 1.034837 | 0.996269 | 0.877660 | 1.996815 |
| warm | str_methods | 0.997247 | 1.056773 | 0.996154 | 1.948911 | 2.164927 |
| warm | list_ops | 1.067720 | 1.078506 | 0.998399 | 14.366912 | 1.955068 |
| warm | attr_access | 1.008335 | 0.997247 | 0.996904 | 3.023828 | 2.012500 |
| warm | call_overhead | 1.013583 | 0.996369 | 0.997435 | 9.090687 | 2.016598 |
| warm | jitkernels | 0.989630 | 1.042230 | 0.996860 | 0.829058 | 1.970010 |

## Controlled startup

Each condition uses 31 alternating paired cycles after a discarded cycle. Native compilation is disabled. Both direct predecessor comparisons are retained.

| Condition | Reference | Elapsed/reference | CPU/reference | RSS/reference |
|---|---|---:|---:|---:|
| startup_matched | Range formula | 0.998697 | 1.002953 | 1.006459 |
| startup_matched | Ownership | 1.001797 | 1.005262 | 1.005872 |
| startup_matched | CPython | 1.396557 | 1.444860 | 1.864776 |
| startup_relocated | Range formula | 0.980991 | 0.978979 | 1.008284 |
| startup_relocated | Ownership | 0.999041 | 1.001073 | 0.997654 |
| startup_relocated | CPython | 1.389638 | 1.436177 | 1.853871 |
| no_site_matched | Range formula | 1.033428 | 1.028183 | 0.994962 |
| no_site_matched | Ownership | 1.013449 | 1.014317 | 1.002555 |
| no_site_matched | CPython | 0.629003 | 0.596061 | 1.555848 |
| imports_matched | Range formula | 1.001093 | 0.999965 | 1.000401 |
| imports_matched | Ownership | 1.005987 | 1.005519 | 1.000000 |
| imports_matched | CPython | 2.809941 | 2.971953 | 2.525784 |
| imports_relocated | Range formula | 0.970597 | 0.970373 | 1.010365 |
| imports_relocated | Ownership | 1.006380 | 1.006641 | 0.977546 |
| imports_relocated | CPython | 2.826732 | 2.999264 | 2.469161 |

## Full census

Five alternating paired cycles follow a discarded cycle. Aggregates are geometric means of the per-fixture median paired ratios. There are 23 workload timers, excluding startup, and 24 process elapsed, CPU, and actual OS peak-RSS measurements.

| Metric | JIT/checkpoint | Interpreted/checkpoint | JIT/ownership | Interpreted/ownership | JIT/CPython | Interpreted/CPython |
|---|---:|---:|---:|---:|---:|---:|
| ns | 0.840021 | 0.975584 | 1.005769 | 1.013171 | 3.611696 | 9.743572 |
| wall_ns | 0.889381 | 0.982041 | 0.998130 | 1.015194 | 3.393923 | 5.940025 |
| cpu_ns | 0.888144 | 0.982693 | 0.999195 | 1.015146 | 3.478877 | 6.159913 |
| rss_bytes | 0.948192 | 0.942816 | 0.992029 | 0.999139 | 2.099764 | 1.934655 |

JIT workload-time wins against CPython: 6/23. Peak-RSS wins: 0/24. Superiority across every meaningful metric remains unachieved.

| Fixture | JIT time/ownership | Interpreted time/ownership | JIT RSS/ownership | JIT time/CPython | JIT RSS/CPython |
|---|---:|---:|---:|---:|---:|
| fannkuch | 0.999510 | 1.000436 | 0.997294 | 9.130955 | 1.981681 |
| nbody | 1.012897 | 1.029623 | 0.996797 | 9.038149 | 1.980932 |
| fib | 1.006900 | 0.996265 | 0.998926 | 3.009398 | 2.014100 |
| pidigits | 0.988759 | 1.002883 | 0.998957 | 0.888194 | 1.996888 |
| pyaes | 1.013646 | 1.029634 | 0.997871 | 0.671438 | 2.003195 |
| richards | 1.011277 | 0.995550 | 0.999461 | 8.376318 | 1.993555 |
| sumvm | 0.999092 | 1.010147 | 0.998924 | 0.057743 | 2.013001 |
| nested_loops | 0.989764 | 0.998261 | 1.001611 | 0.096001 | 2.020585 |
| jitloop | 1.008232 | 1.000395 | 0.996791 | 0.073772 | 2.016234 |
| jitkernels | 0.994173 | 1.049328 | 0.997866 | 0.876628 | 1.995731 |
| deltablue | 1.001167 | 0.997008 | 0.997782 | 19.745531 | 2.172947 |
| float_math | 1.005046 | 1.007113 | 0.999697 | 7.657199 | 2.996372 |
| spectral_norm | 1.000216 | 1.009542 | 0.996285 | 2.165220 | 2.011790 |
| json_bench | 0.999085 | 1.005829 | 0.926449 | 1.146997 | 2.569516 |
| str_methods | 1.006353 | 1.041247 | 0.996572 | 2.018127 | 2.193096 |
| dict_ops | 1.024972 | 1.029816 | 0.997843 | 5.616213 | 1.975375 |
| list_ops | 1.017386 | 1.064923 | 0.996759 | 14.331828 | 1.973233 |
| attr_access | 1.005245 | 1.007842 | 0.998424 | 3.159886 | 2.038710 |
| call_overhead | 1.010014 | 0.991839 | 0.999478 | 9.223697 | 2.056928 |
| generators | 1.002311 | 1.015456 | 0.996797 | 9.730484 | 2.008593 |
| deque_ops | 1.015703 | 1.011045 | 0.992219 | 16.983347 | 2.021365 |
| datetime_ops | 1.017281 | 0.997109 | 0.995542 | 154.018004 | 2.152941 |
| pickle_bench | 1.004552 | 1.015524 | 0.934246 | 333.909687 | 2.481818 |
| startup | 0.981369 | 1.021154 | 1.000544 | 1.436569 | 1.997831 |

## Diagnostics and limits

The standalone capacity diagnostic links against libraries whose hashes exactly match the candidate layout record. It reads 56 prepared frozen artifacts, decodes them directly, and compares the graph returned by the actual relocated cache loader. Requested vector capacity is not RSS. Its output is:

```text
decoded_code_count=2703,cached_code_count=2703
field,decoded_len_bytes,decoded_capacity_bytes,cached_len_bytes,cached_capacity_bytes
cellvars,11112,33120,11112,11112
coltable,1478088,2410344,1478088,1478088
const_identifiers,0,0,0,0
constant_container,73952,74112,73952,74112
constants,367168,367168,367168,367168
exception_table,45760,70800,45760,45760
freevars,8016,18720,8016,8016
hidden_locals,120,384,120,120
instructions,985392,985392,985392,985392
linetable,492696,803448,492696,492696
names,402792,588096,402792,402792
no_interrupt_jumps,916,2112,916,916
varnames,189360,311520,189360,189360
wire_marks,111782,123174,111782,111782
decoded_spare_vector_bytes=1621236,cached_spare_vector_bytes=160
```

Idle-process vmmap captures after importing os and time are diagnostic snapshots, not paired peak-RSS measurements. Raw reports are included. Disassembly comparisons are also diagnostic and cannot by themselves establish a cause for timing shifts.

Cold stdlib extraction, parallel throughput, explicit GC-pause distributions, energy, and controlled build latency are not repeated here. CPython is the recorded 3.14.7 GIL build; WeavePy native execution remains gated when the GIL is disabled. The illustrative fannkuch fixture is not canonical fannkuch, and pyaes is an XOR scrambler. All preceding archives remain unchanged.
