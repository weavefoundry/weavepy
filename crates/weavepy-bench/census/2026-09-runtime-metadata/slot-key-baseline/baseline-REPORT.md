# Retained-instance and slot-name baseline

This is a baseline investigation of the retained field-validation release 8f1c86d7. No slot-key optimization is included. Runtime sources and identities are referenced from the completed datetime-fields archive.

Eleven workloads replace a retained batch of 10,000 instances. Workload time includes cleanup of the preceding batch and construction of its replacement. The new batch remains live through the timer and process exit. Seven paired samples follow a discarded cycle; per-binary frozen caches stay unchanged. Separate checks compare all values of 33 instances before and after warmup in CPython, JIT, interpreted, and GIL-disabled modes. Timing and OS CPU/peak RSS are measured outside the tool filesystem sandbox.

| Workload | JIT time/CPython | Interpreter time/CPython | JIT RSS/CPython | Interpreter RSS/CPython |
|---|---:|---:|---:|---:|
| slots_1 | 14.3257 | 12.9429 | 2.0901 | 1.9559 |
| slots_2 | 15.4834 | 13.9614 | 2.1541 | 2.0283 |
| slots_8 | 20.3873 | 18.8745 | 2.3792 | 2.2453 |
| slots_9 | 22.6713 | 21.0475 | 2.5401 | 2.3952 |
| slots_16 | 26.1516 | 23.3674 | 3.4402 | 3.2968 |
| dict_2 | 14.2380 | 13.2620 | 2.2065 | 2.0790 |
| dict_10 | 15.7919 | 15.5332 | 2.5956 | 2.4667 |
| retained_date | 33.5124 | 33.7152 | 2.3668 | 2.2347 |
| retained_time | 41.2816 | 41.5597 | 2.5482 | 2.4147 |
| retained_datetime | 39.3887 | 39.4219 | 2.9980 | 2.8647 |
| retained_timedelta | 9.8699 | 9.9314 | 2.5138 | 2.3818 |

Live-allocation diagnostics use MallocStackLogging and malloc_history -allBySize -fullStacks in separate processes. They measure current live allocations, not total allocation/free churn or execution speed. Each run first warms 20 objects, then retains either zero or 3,000 objects. Source and disassembly correlate SlotStorage::insert return offsets 260, 448, 692, 1164, and 1636 with name allocation and copying. Mach-O bindings identify the calls as malloc and memcpy. Container allocations have separate sites.

Subtracting the empty-batch counts gives exactly 27,000 name allocations for 3,000 nine-slot instances and 30,000 for 3,000 datetimes. Instrumented sizes are 864,000 and 1,056,000 bytes, respectively. These byte counts are not predictions of uninstrumented RSS savings. The generic interpreter slot-store cache uses class-version and key/index guards before insertion; ordinary first assignments can specialize. The next experiment can share existing code-name strings at that guarded insertion point.

Preserved diagnostic corrections: an ad hoc full-text regular expression scan was stopped with SIGTERM after taking too long, without overlapping any benchmark. Its raw histories were unchanged. The first streaming summarizer missed singular "1 call" headers; its original script and JSON remain diagnostic-only. The corrected separate summarizer handles singular and plural headers. The first batch verifier then stopped before any timing because datetime.py shadowed the standard library. A fresh input set prefixes those fixture names with retained_; all 11 timed source hashes remain unchanged. Original failures and corrected inputs are retained.

Both strong performance gaps and implementation uncertainty remain. Sharing keys has not yet been implemented or measured. The source-level opportunity does not establish its eventual speed or memory benefit.
