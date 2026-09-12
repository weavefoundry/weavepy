# Decode supported pickle data natively

Module-level pickle.loads can decode protocol 4/5 built-in scalar, list, tuple, and dict graphs in Rust. A first pass checks supported instructions, frame bounds, container types, aliases, cycles, and depth without constructing Python objects. A second pass constructs the accepted acyclic graph. Every mutable container is registered with the collector, including nested containers that can acquire cycles through later mutation.

Unsupported instructions, legacy protocols, surrogate strings, tuple/float dictionary keys, cycles, deep graphs, configured buffers, and custom reconstruction use the existing unpickler. Encoding remains unchanged. Weak guards cover reader-class identity/version, init/load functions and their code/default overrides, and the ordered dispatch table and handler code/defaults. They preserve tested dispatch and method-code changes without retaining a temporary class. Private module-global implementation changes are not universally guarded.

All 38 compiler tests, 328 VM tests, 52 JIT tests, 275 positive compatibility checks, and static checks pass. The additional differential compares 1,546 streams with CPython and the preceding release in JIT-enabled, interpreted, and GIL-disabled modes. Each candidate mode accepts 282 streams natively, with exact CPython values, container aliases, types, float bits, and dictionary order. Fallback outcomes match the preceding release. There are 58 existing malformed-input error-message differences per mode, all with the same UnpicklingError type as CPython.

The release SHA-256 is `247f3eb9b1c1bde87c77ccb9958c7b80430ef4bf652b3facd99408a70a1502cd` and its size is 44,250,016 bytes, 38,000 bytes larger than the preceding empty-table release. All 12 measured object layouts are unchanged. The source snapshot captures 131 sources and 37 measurement inputs. No new unsafe code is introduced by the decoder.

Measurements run outside the tool filesystem sandbox on the recorded CPython 3.14.7 GIL build. Order alternates across paired cycles, with one discarded preparation cycle. Ratios below one mean less time or memory. Peak RSS and process CPU time come from OS wait4 resource usage. No tracemalloc or object-size estimate is substituted for process memory.

## Focused encoding and decoding

Seven measured warm cycles. Four component fixtures preserve the actual census payload and Record/Point class definitions, with 20 protocol-5 operations per invocation. Four larger decode fixtures use prebuilt CPython files and one operation per invocation: lists and tuples with 200,000 integers, 10,000 nested dictionary/list rows, and 8 MiB of bytes. Values and class identities are verified for all five timed variants before measurement. Candidate native acceptance is required for supported payloads and rejection for the custom record graph.

| Probe | Mode | Work time/previous | Process time/previous | CPU/previous | RSS/previous | Work CPU/previous |
|---|---|---:|---:|---:|---:|---:|
| payload_dumps | jit | 0.9901 | 1.0012 | 0.9993 | 1.0051 | 0.9857 |
| payload_dumps | interp | 1.0054 | 0.9504 | 0.9963 | 0.9721 | 1.0058 |
| payload_loads | jit | 0.0095 | 0.1830 | 0.1845 | 0.9983 | 0.0099 |
| payload_loads | interp | 0.0114 | 0.1975 | 0.1914 | 0.9738 | 0.0114 |
| records_dumps | jit | 0.9838 | 0.9713 | 0.9875 | 1.0109 | 0.9930 |
| records_dumps | interp | 0.9635 | 0.9647 | 0.9881 | 0.9819 | 0.9865 |
| records_loads | jit | 1.0045 | 0.9997 | 0.9994 | 1.0125 | 1.0014 |
| records_loads | interp | 1.0137 | 0.9989 | 0.9963 | 0.9772 | 1.0142 |
| large_int_list | jit | 0.0013 | 0.0230 | 0.0232 | 0.9715 | 0.0013 |
| large_int_list | interp | 0.0019 | 0.0302 | 0.0290 | 0.9448 | 0.0019 |
| large_int_tuple | jit | 0.0020 | 0.0280 | 0.0271 | 0.9287 | 0.0021 |
| large_int_tuple | interp | 0.0026 | 0.0303 | 0.0296 | 0.9164 | 0.0027 |
| large_dicts | jit | 0.0106 | 0.0416 | 0.0403 | 0.9341 | 0.0106 |
| large_dicts | interp | 0.0134 | 0.0483 | 0.0469 | 0.9024 | 0.0134 |
| large_bytes | jit | 0.2436 | 0.9657 | 0.9588 | 0.7725 | 0.2453 |
| large_bytes | interp | 0.2477 | 0.9158 | 0.9255 | 0.7506 | 0.2519 |

| Probe | Mode | Work time/CPython | Process time/CPython | CPU/CPython | RSS/CPython | Work CPU/CPython |
|---|---|---:|---:|---:|---:|---:|
| payload_dumps | jit | 876.575 | 22.435 | 23.694 | 2.385 | 867.556 |
| payload_dumps | interp | 709.138 | 18.879 | 20.286 | 2.194 | 705.859 |
| payload_loads | jit | 4.479 | 2.714 | 2.918 | 2.345 | 4.543 |
| payload_loads | interp | 4.220 | 2.490 | 2.691 | 2.157 | 4.363 |
| records_dumps | jit | 398.470 | 33.351 | 35.372 | 2.404 | 381.951 |
| records_dumps | interp | 349.391 | 29.584 | 31.937 | 2.201 | 341.984 |
| records_loads | jit | 354.604 | 18.685 | 21.777 | 2.428 | 377.757 |
| records_loads | interp | 332.544 | 16.464 | 19.060 | 2.200 | 323.495 |
| large_int_list | jit | 0.446 | 1.864 | 1.882 | 1.806 | 0.456 |
| large_int_list | interp | 0.490 | 1.704 | 1.744 | 1.693 | 0.466 |
| large_int_tuple | jit | 0.814 | 2.010 | 1.994 | 2.127 | 0.822 |
| large_int_tuple | interp | 0.835 | 1.828 | 1.910 | 2.027 | 0.814 |
| large_dicts | jit | 3.233 | 2.342 | 2.433 | 2.232 | 3.234 |
| large_dicts | interp | 3.234 | 2.259 | 2.373 | 2.081 | 3.234 |
| large_bytes | jit | 1.110 | 2.074 | 2.170 | 1.715 | 1.109 |
| large_bytes | interp | 0.950 | 2.000 | 2.084 | 1.636 | 0.949 |

The focused tables above compare the retained decoder directly with the preceding empty-table release. Encoding is unchanged, and custom record reconstruction uses the existing Python implementation. Large integer list/tuple workload timers can beat CPython while whole-process time and memory remain worse. Isolated timer wins do not establish an overall performance win. All unfavorable movements remain in the tables.

## Full census

Five measured cycles compare checkpoint 9a69c41, the preceding empty-table release (988d15d1), and CPython. Startup is excluded only from the isolated workload-time aggregate.

| Metric | JIT/checkpoint | Interpreter/checkpoint | JIT/previous | Interpreter/previous | JIT/CPython | Interpreter/CPython |
|---|---:|---:|---:|---:|---:|---:|
| Workload time, 23 fixtures | 0.799 | 0.952 | 1.003 | 0.995 | 3.558 | 9.808 |
| Process time, 24 fixtures | 0.853 | 0.963 | 0.998 | 0.994 | 3.268 | 5.784 |
| CPU time, 24 fixtures | 0.866 | 0.965 | 0.998 | 0.997 | 3.351 | 5.986 |
| Peak RSS, 24 fixtures | 0.938 | 0.934 | 0.999 | 1.008 | 2.082 | 1.916 |

WeavePy wins 6/23 workload timers and 0/24 peak-RSS comparisons. The objective of beating CPython across every meaningful metric remains unachieved.

| Workload | JIT time/previous | Interpreter time/previous | JIT RSS/previous | JIT time/CPython | JIT RSS/CPython |
|---|---:|---:|---:|---:|---:|
| fannkuch | 0.993 | 1.014 | 1.004 | 9.833 | 1.947 |
| nbody | 1.064 | 1.034 | 1.004 | 11.316 | 1.968 |
| fib | 1.053 | 1.034 | 1.009 | 2.973 | 1.992 |
| pidigits | 1.012 | 0.996 | 1.014 | 0.923 | 1.989 |
| pyaes | 0.992 | 1.004 | 1.006 | 0.651 | 1.982 |
| richards | 1.002 | 0.944 | 1.004 | 7.618 | 1.976 |
| sumvm | 1.008 | 1.016 | 0.999 | 0.057 | 1.987 |
| nested_loops | 0.984 | 0.996 | 1.002 | 0.078 | 1.986 |
| jitloop | 0.996 | 1.015 | 0.999 | 0.073 | 1.991 |
| jitkernels | 1.004 | 1.088 | 1.004 | 0.894 | 1.977 |
| deltablue | 1.033 | 1.010 | 1.003 | 21.898 | 2.156 |
| float_math | 1.004 | 0.997 | 1.001 | 7.316 | 2.991 |
| spectral_norm | 1.005 | 1.018 | 1.006 | 2.136 | 1.990 |
| json_bench | 0.977 | 1.006 | 0.937 | 1.170 | 2.529 |
| str_methods | 1.002 | 1.017 | 1.004 | 2.009 | 2.177 |
| dict_ops | 1.003 | 1.016 | 1.003 | 5.833 | 1.956 |
| list_ops | 1.013 | 1.040 | 1.007 | 13.857 | 1.972 |
| attr_access | 0.991 | 0.992 | 1.002 | 2.820 | 2.029 |
| call_overhead | 1.046 | 0.955 | 0.998 | 7.782 | 2.030 |
| generators | 1.056 | 1.024 | 1.003 | 8.583 | 1.978 |
| deque_ops | 1.021 | 0.949 | 1.000 | 23.352 | 2.006 |
| datetime_ops | 0.886 | 0.872 | 1.003 | 108.564 | 2.134 |
| pickle_bench | 0.949 | 0.877 | 0.968 | 346.741 | 2.530 |
| startup | 1.009 | 0.968 | 1.005 | 1.324 | 1.976 |

## Controlled startup and imports

Thirty-one paired cycles with JIT disabled, equal-length executable paths, isolated caches, and verified matching or relocated serialized code filenames. Prepared frozen artifacts remain unchanged throughout timing. Pickle imports are checked separately from the standard import group.

| Case | Elapsed/previous | CPU/previous | RSS/previous | Elapsed/CPython | CPU/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|---:|
| startup_matched | 1.013 | 0.998 | 1.005 | 1.379 | 1.405 | 1.829 |
| startup_relocated | 1.003 | 1.000 | 1.005 | 1.363 | 1.395 | 1.828 |
| no_site_matched | 0.998 | 1.012 | 0.995 | 0.592 | 0.574 | 1.546 |
| imports_matched | 1.001 | 0.994 | 1.001 | 2.759 | 2.889 | 2.428 |
| imports_relocated | 0.999 | 0.998 | 1.001 | 2.793 | 2.921 | 2.425 |
| pickle_import_matched | 1.007 | 1.009 | 1.004 | 2.191 | 2.290 | 2.225 |
| pickle_import_relocated | 1.006 | 1.004 | 1.004 | 2.201 | 2.306 | 2.232 |

## Validation history and limits

The initial integration test replacement method referenced the wrong globals after assigning __code__; its failing source and log are preserved. The corrected test uses the function's retained module globals. The first full VM run exposed a deep fixture's dependence on a process-wide recursion limit changed by another test. The revised fixture constructs a deep pickle stream directly and checks its decoded result iteratively. The GC audit added explicit nested-container registration and a later-mutation cycle test. A required Clippy change replaces a constant-sized chunks_exact call with as_chunks. The final full build and validation use the corrected sources.

GIL-disabled correctness checks do not establish free-threaded performance. Energy use, controlled build latency, every Python workload, and every compatibility edge are not measured. The binary-size increase is retained explicitly. This report does not establish universal performance superiority. The next large bottlenecks include encoding, custom reconstruction, datetime, startup, and process memory.

The focused decoder gains do not translate into an aggregate JIT workload-time win in this census: JIT time rises about 0.35 percent, while interpreted time falls about 0.48 percent versus the preceding release. Process elapsed time falls about 0.19 and 0.56 percent, respectively. JIT peak RSS is nearly unchanged; interpreted peak RSS rises about 0.79 percent. N-body, Fibonacci, and generator JIT timers regress about 6.4, 5.3, and 5.6 percent, and the interpreted JIT-kernel control regresses about 8.8 percent. Those controls and datetime have no implementation changes in this stage. Their measured movements are retained without attributing all variation to the decoder or to measurement noise.

## Small inputs and fallback controls

Seven paired cycles, 2,000 calls after warmup. These compare directly with the preceding empty-table release, including legacy protocols and configured encoding/buffers.

| Probe | Mode | Work time/previous | Process time/previous | CPU/previous | RSS/previous | Work CPU/previous |
|---|---|---:|---:|---:|---:|---:|
| none_p0 | jit | 1.0336 | 1.0483 | 1.0206 | 1.0108 | 1.0115 |
| none_p0 | interp | 1.0181 | 1.0009 | 0.9864 | 0.9723 | 1.0212 |
| none_p5 | jit | 0.0756 | 0.3181 | 0.3152 | 0.9978 | 0.0798 |
| none_p5 | interp | 0.0728 | 0.3067 | 0.2974 | 0.9777 | 0.0758 |
| list_p2 | jit | 0.9982 | 0.9982 | 1.0014 | 1.0086 | 1.0039 |
| list_p2 | interp | 1.0487 | 1.0222 | 1.0110 | 0.9702 | 1.0249 |
| list_p5 | jit | 0.0540 | 0.1796 | 0.1785 | 0.9957 | 0.0554 |
| list_p5 | interp | 0.0562 | 0.1894 | 0.1862 | 0.9653 | 0.0561 |
| list_encoding | jit | 1.0250 | 1.0386 | 1.0387 | 1.0039 | 1.0230 |
| list_encoding | interp | 1.0496 | 1.0269 | 1.0047 | 0.9706 | 1.0280 |
| list_buffers | jit | 0.9729 | 0.9883 | 0.9923 | 1.0047 | 0.9726 |
| list_buffers | interp | 1.0148 | 0.9929 | 0.9961 | 0.9719 | 1.0121 |

## Guard iterations and retained regressions

The initial decoder checks every dispatch function before inspecting the protocol. That adds about 5 to 7 percent to the legacy None control. Recording used opcodes with a bitmask avoids unnecessary short-input guards but slows large integer lists by about 13 to 16 percent. Boolean opcode flags still slow those lists by about 10 to 11 percent. Both trials remain in the archive.

The retained decoder records opcode usage only for streams of at most 512 bytes. Larger streams compile out that bookkeeping and check all reader guards before parsing. An intermediate version checked those guards after parsing and made an immediately returning replacement reader about 20 times slower. The final custom-reader control is within about 1.5 percent of the initial decoder in both modes. Boundary streams of 511, 512, 513, and 4,104 bytes are checked. This reader-customization control is specific to WeavePy and makes no CPython performance claim.

A 15-cycle repeat compares the final release with the initial native decoder (7affa581c). The single-decode tuple slowdown does not repeat, but batches of five tuple decodes still take about 5 percent longer. Byte decoding does not consistently beat CPython. Encoding control movement is retained but cannot be attributed to an encoder change.

| Repeat | Mode | Work time/initial decoder | Work CPU/initial decoder | RSS/initial decoder | Work time/CPython | RSS/CPython |
|---|---|---:|---:|---:|---:|---:|
| large_int_tuple_n1 | jit | 0.9859 | 1.0088 | 0.9994 | 0.9053 | 2.1320 |
| large_int_tuple_n1 | interp | 0.9995 | 1.0130 | 0.9970 | 0.8596 | 2.0267 |
| large_int_tuple_n5 | jit | 1.0478 | 1.0449 | 0.9975 | 0.9103 | 2.5161 |
| large_int_tuple_n5 | interp | 1.0539 | 1.0396 | 0.9976 | 0.9249 | 2.4548 |
| large_bytes_n1 | jit | 0.9241 | 0.9235 | 0.9988 | 1.0093 | 1.7113 |
| large_bytes_n1 | interp | 0.9794 | 0.9772 | 0.9988 | 1.0779 | 1.6349 |
| large_bytes_n5 | jit | 0.9681 | 0.9634 | 0.9985 | 1.2791 | 1.5650 |
| large_bytes_n5 | interp | 0.9894 | 0.9882 | 0.9982 | 1.2430 | 1.5049 |
| payload_loads_n20 | jit | 0.9743 | 1.0097 | 0.9983 | 4.6539 | 2.3409 |
| payload_loads_n20 | interp | 1.0194 | 1.0192 | 0.9968 | 4.6914 | 2.1596 |
| records_dumps_n20 | jit | 0.9292 | 0.9373 | 0.9979 | 320.5569 | 2.3978 |
| records_dumps_n20 | interp | 0.9616 | 0.9636 | 0.9968 | 297.5192 | 2.1998 |

The initial decoder, bitmask, Boolean-flag, adaptive, and final guard snapshots are preserved with all raw measurements. Only the initial and retained releases ran the complete 275-check compatibility census. Every intermediate binary passed its compiler, VM, JIT, and static checks and the 1,546-stream differential. Only the retained release ran the new full 24-workload and controlled startup comparisons. The overall objective remains unachieved.
