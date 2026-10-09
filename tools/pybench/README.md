# Realistic workload suite

The bench gate's fixtures (`crates/weavepy-bench/fixtures`) are small and
heavily tuned. This suite measures WeavePy against CPython on code shaped
like real programs, so a gain on the fixtures can't hide a loss elsewhere.
It has three tools:

- `run.py` runs each benchmark in `benchmarks/` under CPython and WeavePy
  (and optionally a base build), interleaving fresh processes. It reports
  warm time (calls after the first), cold time (the first call, including
  compilation), and peak memory, as WeavePy/CPython ratios with geometric
  means. It also checks that every result matches CPython's.
- `microops.py` times about 150 individual operations (calls of each shape,
  attribute kinds, dunder dispatch, container and string methods, common
  stdlib calls) and ranks them by their ratio to CPython.
- `profile.py` samples one benchmark with macOS `sample` and prints the
  hottest functions by self and inclusive time.

```sh
python3 tools/pybench/run.py --weavepy target/release/weavepy
python3 tools/pybench/run.py --weavepy NEW --base OLD --filter go --filter chaos
python3 tools/pybench/microops.py --weavepy target/release/weavepy --filter call_
python3 tools/pybench/profile.py target/release/weavepy raytrace --secs 5
```

Each benchmark module defines `WORK`, its default size, and `bench(n)`,
which must return the same value every time it's called. Sizes are set so
that CPython takes roughly 50 to 150 ms per call on a recent laptop.
Ratios below 1.00 mean WeavePy is faster or smaller.
