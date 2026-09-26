# Lazy class attribute caches

A class's leaf attribute cache now starts with no entry allocation. Its first
fill allocates the existing 32-entry table. Cold reads and cloning an empty
cache allocate nothing. Hot lookup still uses the same two-choice placement,
name identity, and class-version checks. The cache's existing GIL discipline
and strong/weak ownership choices are unchanged.

The embedded storage is reduced from 32 entries to one pointer. An exercised
cache adds one separate table allocation, so both cold and populated caches
need measurement. `tools/bench_class_population.py` retains 10,000 dynamically
created classes. Setting `WEAVEPY_CLASS_CACHE_WARM=1` also creates their
instances and reads inherited methods and class values through two loops.
Both modes assert their results.

## Measurement

The baseline is `97152bb`. The host, CPython 3.14.5 reference, release profile,
paired sampling, and frozen-cache isolation match the
[math-call report](PERFORMANCE-MATH-CALLS.md). Supplemental probes keep their
source hash in the JSON report.

Seven paired cycles give these ratios for the class-population probe.
"Populated" means both attribute-read loops run; it is separate from the
comparison tool's workload-warmup option.

| Default JIT | Workload time | Process CPU | Peak RSS |
| --- | ---: | ---: | ---: |
| Cold caches, modified/baseline | 0.838 | 0.895 | 0.719 |
| Populated caches, modified/baseline | 0.955 | 0.966 | 0.977 |
| Cold caches, modified/CPython | 1.006 | 0.924 | 0.816 |
| Populated caches, modified/CPython | 1.797 | 1.384 | 1.146 |

Cold-cache median peak RSS falls from 38.85 MB to 27.88 MB; CPython uses
34.16 MB. Interpreter-only cold-cache ratios are 0.836 for workload time and
0.693 for RSS. With populated caches, they are 0.963 and 0.979, respectively.
These results establish a focused memory win, not general CPython parity.

The seven-cycle standard-fixture checks give default-JIT workload ratios of
0.984 for attribute access, 1.017 for DeltaBlue, 0.999 for call overhead,
0.990 for string methods, and 1.007 for startup. DeltaBlue's small slowdown
remains visible; the optimization preserves the existing cache strategy but
adds indirection for populated caches.

The three-cycle full suite gives the following geometric means. Workload time
covers 23 fixtures; process metrics include startup for 24 fixtures.

| Metric | JIT/baseline | Interpreter/baseline | JIT/CPython |
| --- | ---: | ---: | ---: |
| Workload time | 0.998 | 0.991 | 1.052 |
| Process elapsed time | 0.997 | 0.988 | 1.351 |
| Process CPU | 0.998 | 0.986 | 1.349 |
| Peak RSS | 0.989 | 0.986 | 1.716 |

Seven-cycle rechecks do not reproduce the initial spectral-norm and call-overhead
slowdowns: their JIT time ratios are 0.975 and 0.982, respectively.

## Validation

The Rust unit tests check cold reads and clones, pointer-sized empty storage,
colliding names, version invalidation, replacement, and value-owner release.
The release CLI build, formatting, targeted VM Clippy, and six benchmark-tool
tests pass. Clippy uses the same two preexisting local exclusions as the
preceding reports. Sixteen regression scripts pass in default-JIT,
interpreter-only, and free-threaded modes (48 runs), covering classes, version
invalidation, MRO changes, shared code, weakrefs, finalization, native calls,
instance dictionary publication, and GC.

```sh
cargo build --release -p weavepy-cli --bin weavepy
WEAVEPY_STDLIB_CACHE="$PWD/target/performance/stdlib-cache" \
python3.14 tools/bench_compare.py \
  --base /path/to/97152bb/weavepy --new target/release/weavepy \
  --probe tools/bench_class_population.py --work 10000 --samples 7 \
  --frozen-cache-root target/performance/fresh-class-caches \
  --out target/performance/class-population.json
```

Repeat with `WEAVEPY_CLASS_CACHE_WARM=1` and fresh cache/output paths for
populated caches. Raw results remain under `target/performance/class-cache-*`.
Standard fixtures, work sizes, CI baselines, and gate thresholds are unchanged.
