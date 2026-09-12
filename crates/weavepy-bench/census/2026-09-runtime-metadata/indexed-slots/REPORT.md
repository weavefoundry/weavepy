# Indexed slot access

The frozen release is `weavepy-runtime-indexed-slots`, SHA-256 `8a1595a7e0d2f4caaa1a2c7372366458bc4635541cfc90f5eb958aff8f46f3ae`, 44,159,152 bytes. It caches each populated slot's ordered position and checks the name before using it. Different assignment orders and deletion retain named lookup or insertion. Class, descriptor, and value-lane guards remain active.

All 208 compatibility checks, 285 VM tests, 49 JIT tests, Clippy, workspace and feature checks, and debug/release execution proofs pass. All four native slot kernels compile with zero repeated exits. Exact source and measurement hashes are in `inputs/`.

Measurements use nine alternating cycles after a discarded cycle. Ratios below one mean less time or memory. The baseline is the exact-name cache release. These focused tables use ratios of sample medians; raw samples and paired comparison statistics are retained. This intermediate stage has no separate complete standard-suite census.

## Allocation and interpreter slot access

| Workload | Interpreter time/base | Interpreter CPU/base | Interpreter RSS/base | Interpreter time/CPython | Interpreter RSS/CPython |
|---|---:|---:|---:|---:|---:|
| retained_slots_1 | 0.998 | 0.994 | 1.000 | 18.305 | 3.509 |
| last_slot_access_1 | 1.001 | 0.987 | 1.001 | 7.904 | 1.941 |
| retained_slots_2 | 0.999 | 0.995 | 1.000 | 17.795 | 3.568 |
| last_slot_access_2 | 0.950 | 0.977 | 0.999 | 7.784 | 1.933 |
| retained_slots_8 | 1.000 | 1.000 | 1.000 | 15.227 | 2.845 |
| last_slot_access_8 | 0.793 | 0.898 | 0.999 | 7.886 | 1.935 |
| retained_slots_9 | 0.968 | 0.970 | 1.000 | 14.093 | 3.160 |
| last_slot_access_9 | 0.913 | 0.975 | 1.002 | 7.766 | 1.940 |
| retained_slots_16 | 0.979 | 0.980 | 1.000 | 15.153 | 4.166 |
| last_slot_access_16 | 0.909 | 0.959 | 1.001 | 7.652 | 1.931 |
| slot_delete_reinsert | 0.969 | 0.992 | 1.000 | 14.245 | 1.940 |
| slot_access_8_index_0 | 0.997 | 0.990 | 1.001 | 8.008 | 1.939 |
| slot_access_8_index_3 | 0.897 | 0.946 | 1.002 | 8.030 | 1.940 |
| alternating_slot_orders | 0.954 | 0.972 | 1.001 | 14.558 | 1.935 |

## Native slot access

| Workload | JIT time/base | JIT CPU/base | JIT RSS/base | JIT time/CPython | JIT RSS/CPython |
|---|---:|---:|---:|---:|---:|
| native_slots_1 | 0.927 | 0.976 | 0.999 | 1.342 | 2.064 |
| native_slots_8 | 0.380 | 0.614 | 1.003 | 1.355 | 2.063 |
| native_slots_16 | 0.578 | 0.787 | 1.003 | 1.370 | 2.060 |
| native_alternating_slot_orders | 0.905 | 0.939 | 1.002 | 1.438 | 2.058 |

## Standard attribute and call controls

| Workload | JIT time/base | JIT CPU/base | JIT RSS/base | JIT time/CPython | JIT RSS/CPython |
|---|---:|---:|---:|---:|---:|
| attr_access | 0.935 | 0.946 | 1.004 | 3.251 | 2.209 |
| deltablue | 1.003 | 1.003 | 1.001 | 19.885 | 2.245 |
| richards | 1.011 | 1.010 | 1.000 | 8.264 | 2.083 |
| call_overhead | 1.016 | 1.017 | 1.007 | 9.087 | 2.134 |

The eighth-field interpreter workload improves about 21%, and its native workload improves about 62%. The broad attribute fixture improves about 7%. The call control regresses about 2% with about 1% more peak RSS; Richards is about 1% slower. These regressions remain in the samples. Allocation memory is approximately unchanged.

An actual compiled-crate layout probe reports a 32-byte InlineCache and a 488-byte CodeObject on this host. Earlier local planning notes estimated 24 bytes per cache slot; the measured layout supersedes that estimate. This candidate does not change the cache enum size.
