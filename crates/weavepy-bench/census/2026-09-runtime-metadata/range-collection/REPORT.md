# Exact range collection

Lists and tuples built from exact ranges with 64-bit bounds now reserve their known capacity once and fill inline integers directly. The VM collector and builtin constructors share the path. Count and cursor arithmetic use 128 bits, including negative steps and the final advance beyond a 64-bit endpoint. Oversized counts raise OverflowError; impossible vector capacities raise MemoryError. Wider bounds and iterator objects retain their existing paths. No new unsafe code, public Rust API, or object layout is introduced.

Candidate SHA-256: `e3d4ee88511093902a6fd162e734ada56365c78b78da23cf12138be4ec2bf1f2`; 44,211,152 bytes, 144 bytes above the preceding string argument release. All 12 measured layouts are unchanged. Frozen inputs contain 117 sources and 37 measurement inputs. The preceding release is 76d06d7a7b0e76779dc79fc8e4bf5dacbe1eb21eed7fec3fb458250568131fb0.

All 316 VM tests, 52 JIT tests, 253 configured compatibility checks, Clippy, and workspace/all-feature/no-JIT checks pass. The expanded compatibility set includes CPython range, iterator, and iterator-length suites. Three focused units cover boundary values, fallback types, impossible capacities, and Python constructor behavior. No huge-allocation oracle was run on the preceding VM.

All 19 bounded release-oracle cases match CPython in native, interpreted, and GIL-disabled modes. They cover empty/ascending/descending ranges, 64-bit endpoints, 128-bit and larger bounds, tuple identity, list tracking, subclasses, tuple.__new__, list.__init__, star unpacking, sorting, and partially consumed iterators. Additional permanent checks cover wrapper iteration, index callbacks, and later list cycles. Five unrelated workload return-value oracles match CPython on first and warmed calls.

The initial test draft incorrectly assumed tuple(custom_iterable) invokes the source length hook once. It failed on CPython before any runtime change: CPython checks the returned iterator, while WeavePy also probes the original source. The raw failure and protocol probes are retained. The corrected test checks values and exactly-once iteration for both constructors and the source length hook for lists, and explicitly documents the existing tuple difference. This experiment does not repair that generic protocol; the final release preserves the preceding protocol output.

Each performance group uses seven alternating paired cycles after a discarded cycle. Ratios below one mean lower time or memory. Process wall time, CPU, and peak RSS include startup and validation. Cold controls omit explicit workload warmup; operating-system caches are not flushed. Controls use the checkpoint as base and the string argument release as previous; tables below use the previous release. Construction and population comparisons use that same preceding release as base.

The small-range group constructs lists and tuples 20,000 times from a reused range at each size. Its timer includes Python loop, call, length, and validation overhead. The retained-population group times one construction only; its process CPU and peak RSS also include checksum validation and the retained contents. Repeated-zero populations are controls with shared small-integer behavior in CPython. Both custom groups use interpreter mode. The original population script labels itself a draft; the archived samples below record its actual execution.

Large list construction takes 20 to 29 percent of the preceding release time, and tuples take 40 to 47 percent. Those construction timers beat CPython at all five measured populations. Peak process RSS improves by up to about 5 percent but remains above CPython at every population: at two million elements, the list ratio is 1.300 and the tuple ratio is 1.093. Small constructions improve by about 5 to 61 percent but remain slower than CPython. Repeated-zero controls remain about four times CPython peak RSS at two million elements. All cold/warm JIT control timings are within approximately 1.1 percent of the preceding release; raw interpreter controls retain their separate results.

The population checksum calls sum(items) after construction. The current VM sum path copies exact list and tuple contents before reducing them, so these whole-process peaks do not isolate container construction. That source-level finding is a follow-up target; this experiment does not measure the copy contribution separately.

## cold

| Fixture | Time/previous | Wall/previous | CPU/previous | RSS/previous | Time/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|---:|
| str_methods | 1.0006 | 1.0007 | 1.0009 | 0.9971 | 2.0271 | 2.1875 |
| list_ops | 0.9928 | 0.9925 | 0.9921 | 0.9984 | 13.7168 | 1.9721 |
| attr_access | 0.9912 | 0.9890 | 0.9890 | 0.9947 | 3.0925 | 2.0289 |
| call_overhead | 1.0012 | 0.9999 | 1.0007 | 0.9979 | 9.1636 | 2.0460 |
| jitkernels | 1.0014 | 0.9934 | 0.9940 | 0.9968 | 0.8778 | 1.9894 |

## warm

| Fixture | Time/previous | Wall/previous | CPU/previous | RSS/previous | Time/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|---:|
| str_methods | 0.9938 | 0.9925 | 0.9924 | 0.9986 | 1.9333 | 2.1566 |
| list_ops | 0.9919 | 0.9932 | 0.9925 | 0.9968 | 13.6851 | 1.9490 |
| attr_access | 0.9894 | 0.9950 | 0.9938 | 0.9995 | 3.0582 | 1.9959 |
| call_overhead | 0.9974 | 0.9968 | 0.9973 | 0.9995 | 9.0162 | 2.0125 |
| jitkernels | 0.9986 | 0.9884 | 0.9920 | 0.9968 | 0.8399 | 1.9576 |

## constructors

| Fixture | Time/previous | Wall/previous | CPU/previous | RSS/previous | Time/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|---:|
| list_0 | 0.9209 | 0.9776 | 0.9764 | 0.9948 | 10.1748 | 1.8436 |
| list_1 | 0.9455 | 0.9737 | 0.9726 | 0.9930 | 8.8449 | 1.8432 |
| list_4 | 0.9134 | 0.9724 | 0.9718 | 0.9959 | 7.6632 | 1.8474 |
| list_16 | 0.7238 | 0.8881 | 0.8829 | 0.9965 | 6.2808 | 1.8490 |
| list_64 | 0.5319 | 0.7635 | 0.7567 | 0.9936 | 3.3085 | 1.8396 |
| list_256 | 0.3864 | 0.5690 | 0.5586 | 0.9937 | 1.6556 | 1.7902 |
| tuple_0 | 0.9168 | 0.9784 | 0.9782 | 0.9948 | 10.2976 | 1.8506 |
| tuple_1 | 0.9284 | 0.9717 | 0.9718 | 0.9959 | 9.5395 | 1.8461 |
| tuple_4 | 0.8817 | 0.9504 | 0.9490 | 0.9971 | 8.5177 | 1.8456 |
| tuple_16 | 0.7371 | 0.8776 | 0.8779 | 0.9982 | 6.7074 | 1.8468 |
| tuple_64 | 0.6325 | 0.7917 | 0.7853 | 0.9959 | 3.6344 | 1.8387 |
| tuple_256 | 0.5114 | 0.6353 | 0.6290 | 0.9931 | 1.5597 | 1.8023 |

## populations

| Fixture | Time/previous | Wall/previous | CPU/previous | RSS/previous | Time/CPython | RSS/CPython |
|---|---:|---:|---:|---:|---:|---:|
| list_range_10000 | 0.1999 | 0.9810 | 0.9815 | 0.9960 | 0.2408 | 1.8189 |
| list_range_100000 | 0.2469 | 0.9737 | 0.9757 | 0.9501 | 0.3024 | 1.7234 |
| list_range_500000 | 0.2827 | 0.9341 | 0.9331 | 0.9689 | 0.3134 | 1.4791 |
| list_range_1000000 | 0.2915 | 0.9139 | 0.9126 | 0.9789 | 0.3156 | 1.3754 |
| list_range_2000000 | 0.2942 | 0.8986 | 0.8946 | 0.9866 | 0.3157 | 1.2995 |
| tuple_range_10000 | 0.4015 | 0.9858 | 0.9831 | 0.9954 | 0.4517 | 1.8116 |
| tuple_range_100000 | 0.4025 | 0.9703 | 0.9693 | 0.9492 | 0.5404 | 1.6485 |
| tuple_range_500000 | 0.4441 | 0.9401 | 0.9376 | 0.9677 | 0.5402 | 1.2598 |
| tuple_range_1000000 | 0.4449 | 0.9204 | 0.9184 | 0.9781 | 0.5569 | 1.1637 |
| tuple_range_2000000 | 0.4699 | 0.9058 | 0.9030 | 0.9868 | 0.5964 | 1.0931 |
| list_repeated_10000 | 0.9850 | 0.9948 | 0.9926 | 0.9988 | 2.8400 | 1.8588 |
| list_repeated_100000 | 1.0094 | 1.0145 | 0.9908 | 0.9950 | 5.0639 | 2.0689 |
| list_repeated_500000 | 0.9815 | 1.0108 | 1.0134 | 0.9966 | 4.5704 | 2.7181 |
| list_repeated_1000000 | 0.9971 | 0.9759 | 0.9948 | 0.9970 | 4.7580 | 3.2813 |
| list_repeated_2000000 | 0.9857 | 0.9747 | 0.9742 | 0.9991 | 4.6142 | 3.9858 |
| tuple_repeated_10000 | 0.9763 | 0.9879 | 0.9860 | 0.9965 | 5.0865 | 1.8590 |
| tuple_repeated_100000 | 1.0017 | 1.0137 | 1.0052 | 0.9960 | 8.8443 | 2.0745 |
| tuple_repeated_500000 | 0.9862 | 0.9993 | 0.9985 | 0.9978 | 8.5461 | 2.7171 |
| tuple_repeated_1000000 | 1.0092 | 0.9895 | 0.9905 | 0.9981 | 8.6549 | 3.2856 |
| tuple_repeated_2000000 | 0.9997 | 0.9976 | 0.9972 | 0.9985 | 8.4108 | 3.9858 |

Every recorded gain, regression, and raw sample is retained. Actual process RSS determines the memory comparison; sys.getsizeof and tracemalloc are not used as physical memory measurements. The full 24-fixture census remains the preceding string argument stage. Dedicated startup/import variants, parallel throughput, and explicit GC-pause distributions are not repeated here. CPython is the recorded 3.14.7 GIL build; native execution remains gated in WeavePy GIL-disabled mode. No controlled build-latency, energy, or free-threaded CPython claim is made. The objective of superiority across every meaningful metric remains unachieved.
