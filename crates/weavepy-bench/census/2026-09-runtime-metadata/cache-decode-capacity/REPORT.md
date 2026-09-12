# Exact allocation of decoded code buffers

The decoder now reserves source-position tables for the expanded instruction count, reserves local-name partitions and marshalled name tuples at their known lengths, and releases all-plain wire markers immediately. This replaces the preceding shrink-after-decoding trial. Both cache loaders still take ownership of uniquely decoded roots and preserve their shared-root copy fallback. Matching and relocated code benefit at decode time. No new unsafe code, object layout, bytecode format, or public Rust API is introduced.

Candidate SHA-256: `43634fbb8b1a5c5d836b1ba59b54b69ab0c3373ea3620d69a975d74b8860bbe4`. Executable size: 44,212,016 bytes, 80 bytes below compaction, 272 bytes below ownership, and 112 bytes above the range-formula release. The snapshot records 126 source files and 37 measurement inputs. All 12 measured layouts are unchanged.

All 38 compiler tests, 323 VM tests, 52 JIT tests, Clippy, workspace/all-feature/no-JIT checks, and 269 positive compatibility checks pass. Exact before/after marshal/import/importlib diagnostics retain the ownership release's outcomes: exact import passes; the configured marshal divergence and importlib failure are unchanged. Native execution, builder, and scalar-leaf proofs pass. Five independent benchmark results match CPython on first and warmed calls in JIT, interpreted, and GIL-disabled parent modes.

Two compiler regressions cover expanded instruction positions and fused wire markers, shared and hidden locals, truncated name/kind pairs, and retained vector capacities. Both initially fail on the preceding allocation behavior. An initial test incorrectly expected a fused instruction to have no wire markers; the corrected test preserves those markers and separately checks plain-marker release. The initial test source and failure logs remain available. Existing cache ownership tests still compare code metadata and shared-copy behavior.

The focused controls recover the compaction trial's large list and interpreted-string slowdowns. Relative to ownership, all ten standard controls use about 1.0 to 1.5 percent less JIT peak RSS. Matching and relocated import probes use about 3.45 percent less peak RSS with similar elapsed time. Native attribute-access timers regress about 2.8 percent in both cold and warm controls. The no-site probe also regresses slightly in time and RSS. These losses remain explicit; the overall CPython objective is unachieved.

All timing uses the declared outside-tool-filesystem-sandbox condition. Standard controls retain the historical shared metadata-cache arrangement and exclude process startup from the workload timer. Supplemental startup probes use equal-length executable paths, independent stdlib metadata and frozen-code caches, verified serialized/runtime filenames, and unchanged frozen artifacts. OS caches are not flushed, so these are warm-cache starts, not first extraction.

## Cold and warm controls

Each condition uses seven alternating paired cycles after a discarded cycle. These focused runs compare ownership as base and compaction as previous; base does not mean checkpoint in this table. Ratios below one indicate a reduction.

| Condition | Fixture | Reference | JIT time/reference | Interpreted time/reference | JIT RSS/reference |
|---|---|---|---:|---:|---:|
| cold | str_methods | Ownership | 1.003164 | 0.988804 | 0.984856 |
| cold | str_methods | Compaction | 1.009093 | 0.953081 | 0.986771 |
| cold | str_methods | CPython | 2.048199 | 2.995705 | 2.171521 |
| cold | list_ops | Ownership | 1.004308 | 1.002647 | 0.989719 |
| cold | list_ops | Compaction | 0.938172 | 0.933969 | 0.988655 |
| cold | list_ops | CPython | 13.542937 | 13.646489 | 1.968784 |
| cold | attr_access | Ownership | 1.027693 | 0.999286 | 0.987882 |
| cold | attr_access | Compaction | 1.016292 | 1.000142 | 0.986856 |
| cold | attr_access | CPython | 3.233758 | 9.387159 | 2.016129 |
| cold | call_overhead | Ownership | 0.996741 | 0.996586 | 0.989551 |
| cold | call_overhead | Compaction | 0.990555 | 0.992880 | 0.987441 |
| cold | call_overhead | CPython | 9.166725 | 10.187816 | 2.018182 |
| cold | jitkernels | Ownership | 0.986111 | 1.000598 | 0.987193 |
| cold | jitkernels | Compaction | 0.999538 | 0.962559 | 0.986674 |
| cold | jitkernels | CPython | 0.874283 | 11.527107 | 1.971215 |
| warm | str_methods | Ownership | 0.999077 | 0.990462 | 0.986513 |
| warm | str_methods | Compaction | 0.998964 | 0.948542 | 0.986513 |
| warm | str_methods | CPython | 1.920342 | 2.990338 | 2.140167 |
| warm | list_ops | Ownership | 1.009972 | 1.007620 | 0.989345 |
| warm | list_ops | Compaction | 0.940527 | 0.949917 | 0.988818 |
| warm | list_ops | CPython | 13.605148 | 13.567196 | 1.937435 |
| warm | attr_access | Ownership | 1.027537 | 0.999900 | 0.987093 |
| warm | attr_access | Compaction | 1.022001 | 1.007281 | 0.991710 |
| warm | attr_access | CPython | 3.163479 | 9.235246 | 1.994797 |
| warm | call_overhead | Ownership | 0.987406 | 0.992683 | 0.986140 |
| warm | call_overhead | Compaction | 0.988244 | 0.991028 | 0.986605 |
| warm | call_overhead | CPython | 8.992315 | 10.025493 | 2.000000 |
| warm | jitkernels | Ownership | 0.999138 | 1.000278 | 0.986359 |
| warm | jitkernels | Compaction | 1.000102 | 0.956452 | 0.987914 |
| warm | jitkernels | CPython | 0.832073 | 11.562989 | 1.949170 |

## Controlled startup

Each condition uses 31 alternating paired cycles after a discarded cycle. Native compilation is disabled. Direct references are the range-formula release, ownership, and CPython.

| Condition | Reference | Elapsed/reference | CPU/reference | RSS/reference |
|---|---|---:|---:|---:|
| startup_matched | Range formula | 1.001016 | 1.000362 | 0.988824 |
| startup_matched | Ownership | 0.995756 | 0.997119 | 0.988242 |
| startup_matched | CPython | 1.371661 | 1.420325 | 1.832427 |
| startup_relocated | Range formula | 0.982880 | 0.980308 | 0.998219 |
| startup_relocated | Ownership | 0.997140 | 1.000179 | 0.989430 |
| startup_relocated | CPython | 1.380179 | 1.431032 | 1.836048 |
| no_site_matched | Range formula | 1.026771 | 1.018800 | 1.003378 |
| no_site_matched | Ownership | 1.009951 | 1.008901 | 1.011895 |
| no_site_matched | CPython | 0.623225 | 0.588990 | 1.567282 |
| imports_matched | Range formula | 0.999423 | 0.998621 | 0.967535 |
| imports_matched | Ownership | 0.996954 | 0.997432 | 0.965545 |
| imports_matched | CPython | 2.817579 | 2.979932 | 2.439148 |
| imports_relocated | Range formula | 0.968400 | 0.967356 | 0.997931 |
| imports_relocated | Ownership | 0.996999 | 0.998220 | 0.965476 |
| imports_relocated | CPython | 2.834296 | 3.002077 | 2.439148 |

## Full census

Five alternating paired cycles follow a discarded cycle. Unlike the focused controls, this run compares checkpoint 9a69c41 and ownership. Aggregates are geometric means of per-fixture median paired ratios. There are 23 workload timers, excluding startup, and 24 process elapsed, CPU, and actual OS peak-RSS measurements.

| Metric | JIT/checkpoint | Interpreted/checkpoint | JIT/ownership | Interpreted/ownership | JIT/CPython | Interpreted/CPython |
|---|---:|---:|---:|---:|---:|---:|
| ns | 0.828725 | 0.962852 | 1.004142 | 1.003565 | 3.552593 | 9.581859 |
| wall_ns | 0.883767 | 0.970600 | 0.995769 | 1.005413 | 3.363690 | 5.883374 |
| cpu_ns | 0.882105 | 0.970555 | 0.996216 | 1.005000 | 3.440034 | 6.084352 |
| rss_bytes | 0.937603 | 0.933997 | 0.981040 | 0.989915 | 2.074714 | 1.914685 |

JIT workload-time wins against CPython: 6/23. Peak-RSS wins: 0/24. Superiority across every meaningful metric remains unachieved.

| Fixture | JIT time/ownership | Interpreted time/ownership | JIT RSS/ownership | JIT time/CPython | JIT RSS/CPython |
|---|---:|---:|---:|---:|---:|
| fannkuch | 0.981136 | 0.995483 | 0.987554 | 9.062630 | 1.967568 |
| nbody | 0.995993 | 1.004101 | 0.983015 | 8.885535 | 1.959916 |
| fib | 1.000984 | 0.996942 | 0.988691 | 2.995389 | 1.982721 |
| pidigits | 0.998946 | 1.001300 | 0.980918 | 0.890263 | 1.969729 |
| pyaes | 1.012233 | 1.018120 | 0.985091 | 0.664563 | 1.972399 |
| richards | 1.005179 | 0.997019 | 0.986037 | 8.381513 | 1.980583 |
| sumvm | 1.000341 | 1.000554 | 0.987614 | 0.056848 | 1.984881 |
| nested_loops | 1.004638 | 1.007930 | 0.987661 | 0.081181 | 1.989201 |
| jitloop | 1.003600 | 1.011869 | 0.990359 | 0.073317 | 1.991389 |
| jitkernels | 0.996853 | 0.988538 | 0.987701 | 0.873611 | 1.973433 |
| deltablue | 1.011913 | 1.000997 | 0.989814 | 19.645493 | 2.153179 |
| float_math | 1.003365 | 1.004648 | 0.995309 | 7.564533 | 2.983666 |
| spectral_norm | 1.000883 | 1.002709 | 0.983589 | 2.098639 | 1.988210 |
| json_bench | 1.003824 | 0.999874 | 0.917874 | 1.157940 | 2.499493 |
| str_methods | 1.005217 | 1.010819 | 0.980920 | 2.028507 | 2.165049 |
| dict_ops | 0.999526 | 1.008606 | 0.987069 | 5.451308 | 1.952077 |
| list_ops | 1.007754 | 1.004685 | 0.990244 | 13.707442 | 1.964516 |
| attr_access | 1.030743 | 0.994020 | 0.986337 | 3.235540 | 2.010741 |
| call_overhead | 0.998617 | 1.002420 | 0.984351 | 8.976578 | 2.024678 |
| generators | 1.012907 | 1.001475 | 0.985577 | 9.504929 | 1.984962 |
| deque_ops | 1.007733 | 1.007457 | 0.985758 | 17.032138 | 2.008978 |
| datetime_ops | 1.012015 | 0.999687 | 0.985149 | 153.627130 | 2.132905 |
| pickle_bench | 1.001746 | 1.023403 | 0.923048 | 328.880130 | 2.446014 |
| startup | 0.986387 | 1.020325 | 0.989663 | 1.425768 | 1.973998 |

## Buffer diagnostic and limits

A standalone diagnostic links against libraries whose hashes exactly match the candidate layout record. It reads the 56 prepared frozen artifacts, decodes them directly, and compares the code graph returned by the actual relocated cache loader. Both graphs contain 26,396 spare vector bytes across 2,703 code objects, versus 1,621,236 before the decoder changes. Remaining spare capacity is in exception tables, no-interrupt jumps, and constant containers. Requested vector capacity is not RSS. Raw output follows.

```text
decoded_code_count=2703,cached_code_count=2703
field,decoded_len_bytes,decoded_capacity_bytes,cached_len_bytes,cached_capacity_bytes
cellvars,11112,11112,11112,11112
coltable,1478088,1478088,1478088,1478088
const_identifiers,0,0,0,0
constant_container,73952,74112,73952,74112
constants,367168,367168,367168,367168
exception_table,45760,70800,45760,70800
freevars,8016,8016,8016,8016
hidden_locals,120,120,120,120
instructions,985392,985392,985392,985392
linetable,492696,492696,492696,492696
names,402792,402792,402792,402792
no_interrupt_jumps,916,2112,916,2112
varnames,189360,189360,189360,189360
wire_marks,111782,111782,111782,111782
decoded_spare_vector_bytes=26396,cached_spare_vector_bytes=26396
```

Cold stdlib extraction, parallel throughput, explicit GC-pause distributions, energy, and controlled build latency are not repeated here. CPython is the recorded 3.14.7 GIL build; WeavePy native execution remains gated when the GIL is disabled. The illustrative fannkuch fixture is not canonical fannkuch, and pyaes is an XOR scrambler. All preceding archives remain unchanged.
