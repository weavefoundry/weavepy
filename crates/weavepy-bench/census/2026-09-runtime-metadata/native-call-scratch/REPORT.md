# Native-call scratch trial

This trial is not retained. It places one-slot argument exchange buffers on the stack in try_native_call. The executable has the same 44,212,016-byte size as the exact-decoder release, but the measured native-call machine-code stack frame grows by 32 bytes, from 1,184 to 1,216 bytes.

All 38 compiler, 324 VM, and 52 JIT tests and static checks pass. The release oracle matches CPython in JIT, interpreted, and GIL-disabled modes. The full 272-check compatibility gate and full 24-workload census were not run for this screened trial.

Seven alternating paired cycles follow a discarded cycle, outside the tool filesystem sandbox. Below one means a reduction relative to the preceding exact-decoder release (43634fbb). All raw samples, including interpreter controls and CPython comparisons, are retained.

| Control | Mode | Time/previous | CPU/previous | RSS/previous |
|---|---|---:|---:|---:|
| cold str_methods | jit | 0.986 | 0.989 | 1.002 |
| cold str_methods | interp | 1.044 | 1.038 | 0.999 |
| cold list_ops | jit | 1.068 | 1.063 | 0.999 |
| cold list_ops | interp | 1.001 | 1.004 | 1.001 |
| cold attr_access | jit | 0.971 | 0.974 | 1.000 |
| cold attr_access | interp | 0.993 | 0.996 | 0.999 |
| cold call_overhead | jit | 0.991 | 0.990 | 1.001 |
| cold call_overhead | interp | 1.013 | 1.013 | 0.996 |
| cold jitkernels | jit | 1.000 | 0.992 | 0.998 |
| cold jitkernels | interp | 1.009 | 1.008 | 0.999 |
| warm str_methods | jit | 0.994 | 0.994 | 1.005 |
| warm str_methods | interp | 1.003 | 1.003 | 1.001 |
| warm list_ops | jit | 1.012 | 1.008 | 0.999 |
| warm list_ops | interp | 0.999 | 1.001 | 1.001 |
| warm attr_access | jit | 0.982 | 0.982 | 0.997 |
| warm attr_access | interp | 1.005 | 1.010 | 0.999 |
| warm call_overhead | jit | 0.990 | 0.991 | 0.999 |
| warm call_overhead | interp | 1.004 | 1.004 | 1.003 |
| warm jitkernels | jit | 0.999 | 0.992 | 0.998 |
| warm jitkernels | interp | 1.014 | 1.012 | 1.001 |

Attribute access improves about 2 to 3 percent, while cold list operations regress about 6.8 percent and cold interpreted string operations regress about 4.4 percent. The cause of these control shifts is not established.

The four original call-shape probes all regress slightly, but their follow-up coverage diagnostic shows they do not exercise the changed native-to-native buffer path: the outer benchmark does not compile; the nested numeric helpers use the existing scalar-leaf path; recursive descend uses generic calls. They remain useful general controls, but they do not establish native-call scratch speed or deep native-stack RSS. The separate release correctness fixture and standard attribute workload do exercise native-to-native calls.

The attribute CPU profiles and pin-pressure diagnostic predate this trial. They identify native-call setup as work to investigate, without establishing the cause of the earlier attribute regression. Waiting launcher thread samples are not VM-thread CPU work. These profiles are not paired performance measurements.

The initial source snapshot omitted the newly untracked regression fixture. Both that initial snapshot and the repaired 127-source snapshot are preserved. The repair adds the unchanged fixture after the build; no runtime source changed. The candidate binary SHA-256 is 7fcca99a42e53733b74dcaaafba2254e4e416404b246917ee36d4cf468931c80.

This screen does not achieve the objective of beating CPython across every meaningful metric.
