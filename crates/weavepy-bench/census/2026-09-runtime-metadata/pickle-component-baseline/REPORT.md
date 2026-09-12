# Pickle component baseline

This is a diagnostic baseline on the empty-table release, not a new optimization. The four components use the actual scalar-container payload and Record/Point class definitions from the standard pickle census fixture. Each timing performs 20 protocol-5 operations after one warm benchmark invocation. Seven alternating paired cycles follow a discarded cycle. Timings run outside the tool filesystem sandbox, with actual OS CPU time and peak RSS.

All eight protocol-4/5 component cases match CPython bytes and reconstructed values in JIT-enabled, interpreted, and GIL-disabled modes. WeavePy decoding also consumes CPython-produced bytes. Record and Point class identities are checked. These cases do not cover every pickle protocol, callback, alias, malformed input, or recursive graph, which any native implementation must test separately.

| Component | Mode | Workload time/CPython | Process elapsed/CPython | CPU/CPython | Peak RSS/CPython |
|---|---|---:|---:|---:|---:|
| payload_dumps | jit | 717.040 | 21.243 | 23.068 | 2.369 |
| payload_dumps | interp | 645.998 | 19.198 | 20.834 | 2.176 |
| payload_loads | jit | 412.811 | 14.773 | 16.024 | 2.350 |
| payload_loads | interp | 360.618 | 12.808 | 13.944 | 2.151 |
| records_dumps | jit | 308.370 | 29.824 | 32.131 | 2.389 |
| records_dumps | interp | 279.479 | 27.123 | 29.372 | 2.193 |
| records_loads | jit | 305.453 | 19.510 | 21.173 | 2.406 |
| records_loads | interp | 266.487 | 17.002 | 18.446 | 2.198 |

JIT-enabled encoding and decoding lose to interpreted execution on all four cases. The counter diagnostics show many framed native entries and generic call fallbacks, without retirement in most cases. Payload encoding, for example, records 113,133 native entries, 65,450 generic dynamic calls, and zero native-to-native calls across the two checked invocations. Compiled functions include write and persistent_id, while the main encoder stays interpreted.

Four separate five-second CPU samples follow three validated warmups. The waiting launcher thread's __ulock_wait samples are not VM work. Payload-encoding VM samples include interpreter dispatch, allocation/free, Object cloning/dropping, and call/frame handling. Sampling and counter runs are diagnostic and are not the paired timer measurements above. Every owned sampled child is terminated and reaped after capture.

The first measurement invocation stopped at the required launch-context metadata assertion before collecting any samples. Its log is retained. The corrected invocation explicitly declared its outside-sandbox launch context. No approval was rejected and no runtime source changed.

The binary SHA-256 is `988d15d15f83af8a51e42028753650b0693ca3dcc130f3081167554e5c5b79d3`. Its exact build sources and full compatibility results are retained in the adjacent native-empty-tables archive. No pickle implementation is changed in this diagnostic stage. The CPython-wide performance objective remains unachieved.
