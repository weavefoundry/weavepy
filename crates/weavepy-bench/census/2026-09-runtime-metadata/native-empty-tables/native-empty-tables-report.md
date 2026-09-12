# Skip empty native callee tables

Ordinary native-to-native calls skip resolved-table lookups when the callee compilation has no function or method tokens. Immutable empty tables cannot gain tokens when another function compiles. Nonempty tables keep their generation checks, and self-calls keep their existing table reuse. The preceding one-slot scratch-buffer trial has been removed; its results remain in the adjacent archive.

Guards, pin lifetimes, buffer pooling, recursion accounting, and exit reconstruction are unchanged. No new unsafe block, public API, or object layout is introduced. All 38 compiler tests, 324 VM tests, 52 JIT tests, 272 positive compatibility checks, and static checks pass. The separate marshal/importlib diagnostics retain the preceding failures. Native-execution checks and first/warm workload returns pass.

The executable SHA-256 is `988d15d15f83af8a51e42028753650b0693ca3dcc130f3081167554e5c5b79d3`, 44,212,016 bytes. Its size and all 12 measured layouts match the preceding exact-decoder release. The actual try_native_call machine-code stack frame remains 1,184 bytes. The frozen input snapshot contains 127 sources and 37 measurement inputs.

Measurements ran outside the tool filesystem sandbox. Ratios are medians of paired samples; aggregate ratios are geometric means across fixtures. Each run alternates variant order and discards a preparation cycle. Below one means lower time or memory. Peak RSS comes from OS wait4 resource usage. CPython is the recorded 3.14.7 GIL build.

## Full census

Five measured cycles, compared with checkpoint 9a69c41, the preceding exact-decoder release (43634fbb), and CPython. Startup has no isolated workload timer and is excluded from that aggregate only.

| Metric | JIT/checkpoint | Interpreter/checkpoint | JIT/previous | Interpreter/previous | JIT/CPython | Interpreter/CPython |
|---|---:|---:|---:|---:|---:|---:|
| Workload time, 23 fixtures | 0.817 | 0.960 | 0.981 | 1.000 | 3.523 | 9.593 |
| Process elapsed time, 24 fixtures | 0.873 | 0.967 | 0.982 | 1.004 | 3.341 | 5.885 |
| CPU time, 24 fixtures | 0.872 | 0.967 | 0.984 | 1.004 | 3.419 | 6.085 |
| Peak RSS, 24 fixtures | 0.937 | 0.928 | 0.992 | 0.995 | 2.072 | 1.903 |

WeavePy wins 6/23 workload timers and 0/24 peak-RSS comparisons. The objective of beating CPython across every meaningful metric remains unachieved.

| Workload | JIT time/previous | Interpreter time/previous | JIT RSS/previous | JIT time/CPython | JIT RSS/CPython |
|---|---:|---:|---:|---:|---:|
| fannkuch | 0.995 | 1.002 | 0.994 | 9.130 | 1.959 |
| nbody | 0.999 | 1.002 | 0.998 | 8.894 | 1.959 |
| fib | 1.000 | 0.998 | 0.995 | 2.908 | 1.980 |
| pidigits | 1.003 | 1.001 | 1.003 | 0.899 | 1.977 |
| pyaes | 0.996 | 1.001 | 0.997 | 0.664 | 1.965 |
| richards | 0.997 | 1.003 | 0.994 | 8.246 | 1.973 |
| sumvm | 1.001 | 1.002 | 0.993 | 0.056 | 1.978 |
| nested_loops | 0.842 | 1.009 | 1.000 | 0.081 | 1.991 |
| jitloop | 1.002 | 0.995 | 0.996 | 0.073 | 1.988 |
| jitkernels | 0.987 | 0.998 | 0.997 | 0.874 | 1.962 |
| deltablue | 0.994 | 0.997 | 0.999 | 19.633 | 2.161 |
| float_math | 0.962 | 1.002 | 1.001 | 7.306 | 2.990 |
| spectral_norm | 0.991 | 1.001 | 0.998 | 2.193 | 1.990 |
| json_bench | 1.005 | 1.007 | 0.933 | 1.155 | 2.478 |
| str_methods | 0.998 | 1.004 | 1.005 | 2.050 | 2.178 |
| dict_ops | 1.004 | 0.991 | 0.992 | 5.501 | 1.946 |
| list_ops | 0.998 | 1.002 | 0.992 | 13.532 | 1.952 |
| attr_access | 0.834 | 0.997 | 1.000 | 2.701 | 2.010 |
| call_overhead | 0.983 | 1.005 | 0.998 | 8.813 | 2.025 |
| generators | 1.007 | 1.005 | 0.997 | 9.759 | 1.980 |
| deque_ops | 0.978 | 0.982 | 0.999 | 16.920 | 2.000 |
| datetime_ops | 1.005 | 1.008 | 0.998 | 153.661 | 2.130 |
| pickle_bench | 1.000 | 1.000 | 0.934 | 336.104 | 2.453 |
| startup | 0.987 | 1.017 | 0.997 | 1.442 | 1.968 |

## Verified native call shapes

Seven measured warm cycles. Correctness and native coverage are checked before timings, through the same registered-module driver shape. The outer loops and called methods must compile, and counters must show more than 100,000 ordinary native calls (excluding the scalar-leaf shortcut). The depth-400 recursive scalar case requires more than 1,000 ordinary native calls. All returned values match CPython.

An initial gate incorrectly required the separately guarded method helper counter for every method. The one/three-inner-call shapes instead use dynamic native bound-method dispatch. The failed gate stopped before timing and is retained under focused-initial, with its exact original script. The corrected gate verifies the common ordinary native setup path rather than one dispatch helper.

| Shape | Mode | Time/previous | CPU/previous | RSS/previous | Time/CPython | RSS/CPython |
|---|---|---:|---:|---:|---:|---:|
| no_inner_args | jit | 0.767 | 0.873 | 0.999 | 3.158 | 1.959 |
| no_inner_args | interp | 1.001 | 0.992 | 0.995 | 10.205 | 1.802 |
| one_inner_arg | jit | 0.986 | 0.988 | 0.996 | 8.866 | 1.998 |
| one_inner_arg | interp | 0.997 | 0.995 | 0.994 | 11.153 | 1.809 |
| three_inner_args | jit | 0.945 | 0.954 | 1.001 | 12.145 | 1.997 |
| three_inner_args | interp | 0.999 | 0.996 | 0.994 | 10.833 | 1.805 |
| recursive_scalar | jit | 1.012 | 1.013 | 0.999 | 9.997 | 2.013 |
| recursive_scalar | interp | 1.000 | 0.989 | 0.994 | 17.092 | 2.047 |

The no-inner-call method timer improves about 23 percent. The recursive scalar control regresses about 1.2 percent, with similar peak RSS. The one/three-inner-call probes still use dynamic bound-method dispatch and remain slower than CPython. Their return-inference limitation is recorded as a separate follow-up, with no unmeasured speedup claim.

## Standard controls

Seven paired cycles per cold/warm fixture, including every interpreter control.

| Control | Mode | Time/previous | CPU/previous | RSS/previous | Time/CPython | RSS/CPython |
|---|---|---:|---:|---:|---:|---:|
| cold str_methods | jit | 1.012 | 1.003 | 1.001 | 2.028 | 2.170 |
| cold str_methods | interp | 1.007 | 1.004 | 0.992 | 2.969 | 1.826 |
| cold list_ops | jit | 0.992 | 0.991 | 0.998 | 13.495 | 1.956 |
| cold list_ops | interp | 0.998 | 1.002 | 0.994 | 13.584 | 1.820 |
| cold attr_access | jit | 0.841 | 0.870 | 1.000 | 2.695 | 2.018 |
| cold attr_access | interp | 1.001 | 1.000 | 0.992 | 9.304 | 1.826 |
| cold call_overhead | jit | 0.972 | 0.973 | 0.999 | 8.854 | 2.024 |
| cold call_overhead | interp | 0.998 | 0.999 | 0.995 | 10.147 | 1.814 |
| cold jitkernels | jit | 0.996 | 0.994 | 0.997 | 0.863 | 1.973 |
| cold jitkernels | interp | 0.997 | 1.001 | 0.994 | 11.496 | 1.809 |
| warm str_methods | jit | 1.001 | 1.000 | 1.003 | 1.945 | 2.144 |
| warm str_methods | interp | 1.005 | 1.006 | 0.997 | 3.037 | 1.805 |
| warm list_ops | jit | 1.001 | 0.998 | 0.996 | 13.501 | 1.933 |
| warm list_ops | interp | 0.999 | 0.998 | 0.997 | 13.571 | 1.798 |
| warm attr_access | jit | 0.828 | 0.849 | 1.001 | 2.613 | 1.989 |
| warm attr_access | interp | 0.998 | 1.000 | 0.995 | 9.148 | 1.797 |
| warm call_overhead | jit | 0.981 | 0.985 | 0.998 | 8.673 | 2.000 |
| warm call_overhead | interp | 1.006 | 1.006 | 0.999 | 9.886 | 1.805 |
| warm jitkernels | jit | 0.995 | 0.990 | 0.999 | 0.829 | 1.949 |
| warm jitkernels | interp | 1.005 | 1.002 | 0.993 | 11.473 | 1.784 |

Attribute-access workload time falls about 16 to 17 percent. The cold JIT string timer rises about 1.2 percent. Small interpreter-control increases also remain in the table. Neither source-level operation counts nor a favorable aggregate establish that every workload improves.

## Controlled startup and imports

Thirty-one paired cycles with JIT disabled, equal-length executable paths, isolated stdlib caches, and verified matching or relocated serialized code filenames. Prepared frozen-code artifacts remain unchanged through measurement. The base is the older range-formula release (337); previous is exact decoder (436).

| Case | Elapsed/previous | CPU/previous | RSS/previous | Elapsed/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|
| startup_matched | 1.001 | 1.000 | 0.993 | 1.382 | 1.820 |
| startup_relocated | 0.998 | 0.997 | 0.991 | 1.384 | 1.818 |
| no_site_matched | 1.012 | 1.008 | 0.991 | 0.624 | 1.555 |
| imports_matched | 1.001 | 1.001 | 0.995 | 2.822 | 2.427 |
| imports_relocated | 1.002 | 1.002 | 0.996 | 2.828 | 2.429 |

All raw samples and complete ratios are retained, including the older-base startup comparison. Cold extraction, parallel throughput, explicit GC-pause distributions, energy, and controlled build latency were not repeated. Native execution remains gated in GIL-disabled mode. No free-threaded CPython comparison or universal performance claim is made.
