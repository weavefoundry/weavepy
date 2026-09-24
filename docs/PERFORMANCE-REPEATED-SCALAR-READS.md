# Repeated scalar attribute reads

The JIT can reuse an immediately repeated numeric field read from the same
local receiver. The first read retains its existing class, storage, and type
guards; the second uses that scalar value. Matching requires identical read
metadata, empty receiver paths, consecutive bytecode positions in one block,
and no intervening NULL-marker boundary. Calls, writes, polling points,
dynamic descriptors, and object/text results retain their existing lowering.
No production unsafe code or object representation change is introduced.

The candidate is compared with runtime `6eae553` on macOS x86_64, Rust 1.94,
and optimized CPython 3.14.5. Separate nonempty frozen caches remain unchanged
during timing. Variants are interleaved, warmup is discarded, and no builds,
tests, profiles, or other benchmarks overlap measurements. Ratios are medians
of matched cycles; lower is better. Cold means the first workload invocation
in a fresh process with a warmed bytecode cache. Warm means the second
invocation in the same process; both invocations remain in process metrics.

## Focused results

Seven-cycle integer probes include instance construction and one million loop
iterations. Workload results are checked against the expected scalar total.

| Probe | JIT/base | Interpreter/base | JIT/CPython |
| --- | ---: | ---: | ---: |
| Dictionary integer, cold | 0.623 | 0.984 | 0.390 |
| Dictionary integer, warm | 0.574 | 1.003 | 0.329 |
| Slot integer, cold | 0.563 | 0.983 | 0.657 |
| Slot integer, warm | 0.592 | 0.998 | 0.561 |

Dictionary integer process elapsed/CPU ratios are 0.860/0.840 cold and
0.773/0.768 warm. Slot integer ratios are 0.764/0.752 cold and 0.713/0.692 warm.
Peak RSS remains within 0.4% of baseline for these cases.

The unchanged standard float workload gives JIT/base ratios of 0.967 cold and
0.992 warm, with interpreter ratios of 0.994/0.983. It still takes 2.752/2.107
times CPython's workload time. A separate seven-cycle recheck gives
JIT/interpreter ratios of 0.965/1.015.

The floating versions of the focused probe remain interpreted in both
binaries. Separate diagnostic traces reject their loops with
`NonUniformLocal(2)`: the accumulator starts as integer zero and then becomes
float. Their JIT-mode ratios are 1.006/0.974 for dictionary storage and
0.914/0.970 for slots, cold/warm respectively. The dictionary cold interpreter
ratio is 1.084. These changes do not demonstrate native floating-loop gains;
the unchanged admission limitation remains explicit. A lower-level JIT test
does exercise actual Float and Bool read reuse, including NaN equality.

A separate seven-cycle numeric diagnostic gives cold JIT/base ratios of
0.992 for sumvm, 0.991 for nested loops, and 0.991 for jitloop. Warm ratios are
0.973, 0.996, and 0.979. This diagnostic preserves the ordinary gate fixtures
and thresholds; it does not resolve earlier Windows gate failures.

## Applications and startup

The unchanged full suite uses three cycles. Workload geometric means include
23 workloads; process means include all 24 fixtures, including startup.

| Metric | JIT/base | Interpreter/base | JIT/CPython |
| --- | ---: | ---: | ---: |
| Workload time | 1.001 | 1.011 | 1.099 |
| Process elapsed | 0.998 | 1.002 | 1.351 |
| Process CPU | 0.999 | 1.001 | 1.304 |
| Peak RSS | 0.996 | 1.003 | 1.569 |

Initial JIT workload ratios include 1.110 for numeric kernels, 1.166 for JSON,
1.131 for dictionaries, and 1.041 for DeltaBlue. Interpreter ratios include
1.105 for generators and 1.094 for deque operations. Seven-cycle rechecks
retain the following costs and gains; they do not replace the full-suite means.

| Recheck | JIT/base | Interpreter/base |
| --- | ---: | ---: |
| Numeric kernels, 2,000 work units | 1.028 | 0.881 |
| Numeric kernels, 20,000 work units | 0.993 | 1.002 |
| JSON, standard workload | 0.904 | 0.988 |
| Dictionaries, 100,000 iterations, cold | 1.177 | 1.029 |
| Dictionaries, 100,000 iterations, warm | 1.061 | 1.027 |
| Dictionaries, 1,000,000 iterations | 1.026 | 1.034 |
| Lists, 10,000 iterations | 1.013 | 1.023 |
| Lists, 1,000,000 iterations | 0.977 | 1.006 |
| Deques, 200,000 iterations | 1.060 | 1.070 |
| Deques, 2,000,000 iterations | 1.010 | 1.014 |
| Generators, standard workload | 0.979 | 1.025 |
| DeltaBlue, standard workload | 1.000 | 1.000 |

The dictionary cost is repeatable, including warmed execution. Separate traces
show that both binaries reject the dictionary function before this lowering
runs; its analysis takes approximately 0.15-0.18 ms in either binary. Those
traces do not explain the workload regression. The cause remains unresolved.
The shorter list recheck increases peak RSS by 4.1%; the longer run gives a
0.998 RSS ratio. Long dictionary, numeric-kernel, and deque RSS ratios are
1.001, 1.008, and 1.002. Longer work does not erase the shorter-case costs.

Two 31-cycle startup sweeps preserve small costs, especially without site.
The second sweep gives the following JIT ratios:

| Startup | Elapsed/base | CPU/base | RSS/base | Elapsed/CPython |
| --- | ---: | ---: | ---: | ---: |
| Normal | 1.011 | 1.001 | 1.004 | 1.448 |
| Without site | 1.020 | 1.041 | 1.009 | 0.828 |
| Isolated | 1.003 | 1.009 | 1.004 | 1.454 |
| Imports | 0.996 | 1.000 | 0.999 | 2.454 |

The first sweep's elapsed/CPU ratios are 1.002/1.011 normal, 1.021/1.034
without site, 1.002/0.998 isolated, and 1.005/1.014 with imports.

The 100-worker probe gives JIT/interpreter workload ratios of 1.016/1.021 and
JIT RSS ratio 0.997. JIT work still takes 4.495 times CPython's time. The
executable is 51,818,728 bytes, 4,184 bytes larger than the preceding runtime.
These results do not establish overall CPython parity.

## Validation and decision

All 70 JIT tests, 376 VM tests, 204 release regression runs, and 14 benchmark-
tool tests pass. Strict JIT Clippy and VM Clippy with existing local exclusions
pass. The new fixture and four probe variants also pass CPython and the
accepted baseline in all three WeavePy modes. Twelve candidate probe checks
pass. All semantic-check timings are excluded from performance comparisons.

The isolated helper test counts one helper invocation for a repeated scalar
read, checks the first guard's original deoptimization position and stack,
and retains separate reads for metadata, receiver, path, opcode, and marker
mismatches. The VM test proves native execution with no deoptimization before
running mutation assertions. Coverage includes type changes, large integers,
NaN, deletion/reinsertion, dictionary reshaping, descriptors, custom attribute
access, intervening calls/writes, aliases, and tracing.

Keep this as a focused improvement: the integer workloads improve by
38-44% cold and 41-43% warm, with a nearly unchanged full-suite geometric
mean. The short dictionary regression and startup costs remain explicit;
this is not an across-the-board improvement. Floating accumulator admission,
dictionary execution, startup, and memory still require further work.

Source, tool, fixture, and executable hashes are
retained under `target/performance/repeated-scalar-attribute-experiment/`,
with raw paired samples in `target/performance/repeated-scalar-*.json`.
The release binary was copied only after its successful build exited.
