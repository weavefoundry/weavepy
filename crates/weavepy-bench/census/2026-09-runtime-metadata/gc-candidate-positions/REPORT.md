# Deferred collector candidate positions

This candidate is deferred and its runtime change is removed from the working implementation. It passes correctness checks, but the background-load diagnostic does not establish a reliable performance gain. Exact sources and every measurement remain available for future investigation.

Baseline: `36989234372e807ab95dc79686d9e5dd3bb51b458a9f7a1d92992357c30373a0`, 44,289,312 bytes. Candidate: `46c0ad28911b504eea8f4c82a0d7589be5de4fad69613655ea29f0faa898bd99`, 44,290,064 bytes (752 bytes larger). The baseline contains the preceding borrowed-handle change, which is validated but has not yet received its own performance comparison.

The candidate replaces collection-local owning handle references with integer positions and uses boxed temporary handles. The main collector function shrinks from 22,988 to 22,088 bytes. Its static ldadd instruction count changes from 17 to 15 and ldaddl from 60 to 50. The discovery closure requests 80 bytes per temporary handle instead of 96. Exact disassembly and malloc stub bindings are preserved. These are static observations, not dynamic counts, execution-time measurements, or proof of an RSS reduction.

Validation passes: 341 VM tests, formatting, Clippy, no-default-features compilation, 33 targeted JIT/interpreter/GIL-disabled checks, and all 275 compatibility checks. The independent collection-graph regression remains in the working implementation.

## Measurement conditions

The original load gate expired after 600 seconds without launching a child or collecting samples. Before any timing, a separate diagnostic was declared with a maximum one- and five-minute load of six, three consecutive ten-second observations, and a fresh output path. The original failed gate and both protocols are preserved. After launch, load rose substantially.

Observed command-phase load ranges (one/five/fifteen minutes): 6.0190 to 19.8623; 4.8369 to 10.1968; 4.7456 to 7.5059. The host has eight logical CPUs and 24 GiB of RAM. Raw VM and swap observations before and after the run are included. Memory conditions also changed substantially, so observed RSS differences do not constitute an isolated causal confirmation.

Each of twelve cases uses seven paired cycles after one warm cycle, alternating variant order. Both releases and CPython pass before/after-warmup checks; candidate GIL-disabled checks also pass. Separate frozen-code caches are unchanged for each release. No samples are excluded or repeated. Timings and RSS are provisional observations under the recorded conditions.

Ratios below one favor the candidate. Work and work CPU cover the fixture timer; wall, process CPU, and peak RSS include setup and shutdown. CPython is the installed 3.14.7 GIL build.

| Group / case | Mode | Work/before | Work CPU/before | Wall/before | CPU/before | RSS/before | Work/CPython | RSS/CPython |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| collections / empty_graph | jit | 1.0411 | 1.0351 | 1.0257 | 1.0229 | 0.9978 | 0.9215 | 1.9456 |
| collections / empty_graph | interp | 0.9997 | 0.9865 | 1.0087 | 0.9981 | 0.9954 | 0.9139 | 1.8103 |
| collections / list_ring | jit | 0.9985 | 0.9907 | 1.0278 | 0.9755 | 0.9969 | 1.1830 | 1.9158 |
| collections / list_ring | interp | 0.9729 | 0.9718 | 1.0073 | 1.0078 | 0.9973 | 1.1657 | 1.7988 |
| collections / dict_ring | jit | 1.0153 | 1.0117 | 1.0030 | 1.0112 | 0.9970 | 1.3043 | 1.9593 |
| collections / dict_ring | interp | 1.0051 | 1.0007 | 0.9880 | 0.9985 | 0.9940 | 1.3173 | 1.8232 |
| collections / slot_tuple_graph | jit | 0.7760 | 0.7979 | 0.8843 | 0.8901 | 0.9941 | 2.6184 | 2.0432 |
| collections / slot_tuple_graph | interp | 0.8732 | 0.8979 | 0.8671 | 0.8605 | 0.9942 | 2.4862 | 1.9084 |
| large / list_30000 | jit | 1.0243 | 1.0216 | 0.9877 | 1.0005 | 0.9925 | 2.4077 | 1.8431 |
| large / list_30000 | interp | 0.9787 | 0.9844 | 0.9797 | 0.9712 | 0.9984 | 2.2671 | 1.7637 |
| large / list_100000 | jit | 1.0449 | 1.0422 | 0.9928 | 1.0216 | 0.9964 | 4.4466 | 1.7287 |
| large / list_100000 | interp | 1.0872 | 1.1081 | 1.0066 | 1.0169 | 0.9998 | 4.7060 | 1.6793 |
| large / slot_tuple_100000 | jit | 0.7845 | 0.7887 | 0.7969 | 0.8206 | 0.9839 | 9.9162 | 2.1288 |
| large / slot_tuple_100000 | interp | 0.8494 | 0.8533 | 0.7782 | 0.8057 | 0.9816 | 9.2825 | 2.0745 |
| large / nested_tuple_10000 | jit | 0.8426 | 0.8406 | 0.8794 | 0.8669 | 0.9772 | 5.6927 | 1.5176 |
| large / nested_tuple_10000 | interp | 0.7722 | 0.7735 | 0.8593 | 0.8633 | 0.9590 | 5.5914 | 1.4358 |
| heaps / ordinary_100000 | jit | 1.3437 | 1.0820 | 1.1165 | 1.0219 | 1.0004 | 8.8288 | 2.8298 |
| heaps / ordinary_100000 | interp | 1.0770 | 1.0146 | 1.0321 | 1.0203 | 1.0106 | 7.7504 | 2.7800 |
| heaps / finalizer_100000 | jit | 0.8996 | 0.9845 | 0.9287 | 0.9313 | 1.0066 | 7.4001 | 2.9634 |
| heaps / finalizer_100000 | interp | 1.0511 | 1.0241 | 0.9496 | 0.9540 | 0.9893 | 7.4940 | 2.8674 |
| heaps / weak_no_callback_100000 | jit | 0.9713 | 0.9777 | 0.9455 | 0.9453 | 0.9995 | 7.8047 | 7.3116 |
| heaps / weak_no_callback_100000 | interp | 1.0542 | 1.0604 | 1.0176 | 1.0431 | 0.9995 | 7.2239 | 7.2483 |
| heaps / weak_callback_100000 | jit | 1.0073 | 1.0071 | 0.9899 | 0.9916 | 0.9998 | 21.6047 | 7.8145 |
| heaps / weak_callback_100000 | interp | 1.0276 | 1.0274 | 1.0298 | 1.0305 | 0.9995 | 21.2209 | 7.7494 |

## Disposition and limits

Temporary-heavy tuple graphs show promising timing and memory observations, while the large list ring and some retained heaps regress. In the 100,000-list case, baseline JIT work ranges from roughly 97 to 704 ms and candidate JIT work from 102 to 816 ms. CPython also varies substantially. A median ratio cannot resolve this interference.

No construction, setter, startup, or full-census acceptance run was performed for this deferred candidate. No energy, controlled build-time, free-threaded CPython throughput, or universal superiority claim is supported. The latest complete full census remains gc-traversal-lists, itself with provisional timing because of host load.

All source snapshots, fixture inputs, validation, raw samples, gate observations, OS context, and machine-code evidence are preserved. Frozen cache files are losslessly compressed; generated bytecode caches are omitted and listed. Executables are identified by hash.
