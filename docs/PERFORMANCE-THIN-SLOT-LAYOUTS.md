# Smaller shared slot layouts

Fixed slot layouts now use the existing one-word `SharedSlice` owner. Reducing
that enum variant lets `SlotStorage` occupy 32 bytes instead of 40, and
`PyInstance` 128 instead of 136 on this 64-bit release. Empty and single-slot
instances retain their inline storage. No additional per-instance allocation,
unsafe implementation, collection-policy change, or JIT admission change is
introduced. The three shared datetime layouts each gain a slice header.

A release-linked layout diagnostic confirms a 144-byte instance allocation,
including its reference counters. Host allocator diagnostics place requests of
144 and 152 bytes in 144- and 160-byte size classes. These observations don't
substitute for whole-process measurements or establish savings on other
allocators.

## Measurements

Comparisons use accepted `fc9e294`, optimized CPython 3.14.5, Intel macOS, ordinary
release settings, alternating paired runs, discarded warmup, and separate
frozen caches verified unchanged during timing. Builds, tests, and profiles
finish before timing. The desktop renderer was active; small differences remain
provisional. All original samples and repeat results are retained.

The new nine-mode instance probe constructs, retains, checks, and releases each
population. Its timer covers construction, checks, and release; process elapsed
time, CPU, and peak RSS include startup and teardown. Modes are selected outside
the construction loop and recorded in benchmark metadata. Seven-cycle runs
cover zero, 10,000, and 100,000 instances; zero-work timings describe setup and
checks, not allocation throughput.

Ratios below are modified/baseline; lower is better. For 100,000 instances:

| Population | JIT workload time | Interpreter workload time | JIT peak RSS | Interpreter peak RSS |
| --- | ---: | ---: | ---: | ---: |
| Empty | 0.993 | 0.931 | 0.962 | 0.957 |
| One dictionary field | 1.041 | 0.986 | 0.974 | 0.973 |
| Eight dictionary fields | 0.990 | 0.996 | 0.980 | 0.982 |
| One slot | 0.976 | 0.980 | 0.963 | 0.951 |
| Eight slots | 1.006 | 0.959 | 0.978 | 0.973 |
| One populated slot of eight | 0.968 | 0.977 | 0.963 | 0.955 |
| Integer subclass | 0.994 | 0.973 | 1.001 | 0.962 |
| String subclass | 1.006 | 0.986 | 1.011 | 0.988 |
| Tuple subclass | 1.013 | 0.998 | 0.969 | 0.962 |

Eleven-cycle independent repeats resolve the one-field timing increase and
support lower native-subclass memory despite the first batch's noisy JIT RSS.
At 100,000 objects, one-field JIT time is 1.002. Integer/string/tuple JIT RSS
ratios are 0.976/0.973/0.976. At 300,000 objects, one-field and native-subclass
JIT RSS ratios are 0.970/0.965/0.967/0.966; JIT times are
1.008/0.992/0.984/0.992. All interpreter RSS ratios in those repeats are below
0.973. Native workloads still take many times CPython's time and memory.

The unchanged 24-fixture suite, three cycles, gives these geometric means.
Workload time excludes its startup fixture; process metrics include it.

| Metric | JIT/baseline | Interpreter/baseline | JIT/CPython |
| --- | ---: | ---: | ---: |
| Workload time | 1.002 | 1.010 | 1.027 |
| Process elapsed | 0.986 | 0.995 | 1.391 |
| Process CPU | 0.990 | 0.996 | 1.253 |
| Peak RSS | 0.987 | 1.001 | 1.553 |

Seven-cycle independent suite repeats reduce the initial dictionary-operation
cost from 1.068/1.073 to 1.035/1.025 for JIT/interpreter. A warmed 500,000-work
run still records 1.028 JIT time and workload CPU, versus 1.004 interpreter.
Warmed datetime at 300,000 records 0.997/0.995, also reflected in workload CPU.

Attribute and call costs remain: warmed attribute access at 500,000 records
1.031/1.035 time, and warmed mixed calls at 750,000 records 1.022/1.012.
These are retained tradeoffs for the smaller instances, not dismissed as noise.
The memory reduction is consistent across ordinary/slot populations and the
larger native-subclass repeats, while broad workload time is approximately
unchanged. This revision is retained for that memory improvement; the remaining
runtime costs and CPython gaps are still optimization targets.

Thirty-one-cycle startup comparisons record JIT elapsed ratios of
0.994/1.015/0.985/1.002 for normal, no-site, isolated, and import startup.
No-site JIT CPU rises to 1.037; normal CPU and RSS are 0.998 and 1.000.
Normal startup remains 1.664 times CPython's elapsed time. This is a memory
improvement, not overall CPython parity or a claim that every metric improves.

## Validation and reproduction

All 395 VM tests pass, including fixed-layout key sharing, independent cloned
values, indexed and hinted updates, small/table conversion, final release,
length mismatches, and size assertions. Embedding on the existing 1 MiB stack,
VM Clippy with the established exclusions, no-default compilation, and
formatting pass. The frozen release passes 306 runs across 102 regression
fixtures, 1,048 semantic probes, fifteen CPython fixtures, and 72 original
population checks. The maintained population probe passes another 180 checks
across CPython and both frozen binaries, including GIL-disabled execution;
all fourteen benchmark-tool tests pass. All 53 probe, three application, and
nine population compilation-decision comparisons remain unchanged.

```sh
cargo build --release -p weavepy-cli --bin weavepy
WEAVEPY_INSTANCE_KIND=slot-eight \
WEAVEPY_STDLIB_CACHE="$PWD/target/performance/stdlib-cache" \
python3.14 -B tools/bench_compare.py \
  --base /path/to/fc9e294/weavepy --new target/release/weavepy \
  --probe tools/bench_instance_populations.py --work 100000 --samples 7 \
  --frozen-cache-root target/performance/fresh-instance-caches \
  --out target/performance/instance-comparison.json
```

The frozen CLI is 51,881,744 bytes, SHA-256
`8ae4be704f32eeb219ba56798d9b3a3ba88100de66b8c146c6e22c259df714d1`.
Its runtime-source patch against `fc9e294`, before adding the maintained probe
and this report, is
`0539c21a8ebf5a3e777894d4a1f428521e0b290e9296f030c72cde601bc8d9fc`.
Sources, allocation diagnostics, profiles, validation, and population results
remain under `target/performance/thin-slot-layout-investigation/`. Full-suite
and startup results use `target/performance/thin-slot-layout-rfc9e294-*`.
