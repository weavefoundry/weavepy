# Deque and datetime performance

This pass continues from commit `4177e850a63ad79f585ad936c5bcb4a63692ebb9`
on `perf/speed-up-json-and-strings`, using CPython 3.14.7 on macOS ARM64.
The goal of beating CPython across every meaningful metric remains unmet.

Relative to the checkpoint, the JIT suite's geometric means use 5.3% less
workload time, 4.5% less process CPU time, and 1.1% less peak RSS. The deque
fixture uses 35.8% less workload time, 34.8% less process CPU time, and 5.2%
less peak RSS. Datetime uses 47.6%, 47.1%, and 19.1% less, respectively.
The interpreter's datetime workload time falls by 49.9%.

The changes aren't uniform wins. Startup elapsed time measures 1.2% higher
with the JIT and 1.8% higher in interpreter mode, and many unrelated small
processes use roughly 0.5% to 1% more peak RSS. The numerical JIT wins over
CPython predate this pass. Deque and datetime still take about 17.7 times
CPython's workload time, and pickle's fixture takes about 344 times as long.

A separate seven-cycle recheck puts `nbody` at 1.005 times baseline JIT
time and `nested_loops` at 0.976 times baseline JIT time. The larger
changes in those unrelated rows of the primary sweep don't repeat, so
they shouldn't be attributed to these library optimizations. The primary
sweep is retained intact, alongside `unrelated-recheck.json`.

<!-- measurements:start -->
## Standard suite

Values are candidate/reference ratios; smaller values mean less time or memory. Geometric means give each fixture equal weight. The workload aggregate includes all 23 timed workloads; the process aggregates include all 24 fixtures, including startup.

| Metric | New/base JIT | New/base interpreter | New/CPython JIT | New/CPython interpreter |
| --- | ---: | ---: | ---: | ---: |
| Timed workload | 0.947 | 0.953 | 3.919 | 9.068 |
| Process elapsed time | 0.956 | 0.954 | 3.526 | 5.516 |
| Process CPU time | 0.955 | 0.955 | 3.620 | 5.726 |
| Peak RSS | 0.989 | 0.988 | 2.202 | 2.042 |

| Fixture | New/base JIT time | New/base interpreter time | New/CPython JIT time | New/CPython JIT RSS |
| --- | ---: | ---: | ---: | ---: |
| `fannkuch` | 1.010 | 1.006 | 10.731 | 2.081 |
| `nbody` | 1.026 | 1.020 | 9.087 | 2.068 |
| `fib` | 0.991 | 1.003 | 2.937 | 2.098 |
| `pidigits` | 0.995 | 1.007 | 0.889 | 2.074 |
| `pyaes` | 1.010 | 0.997 | 11.965 | 2.048 |
| `richards` | 1.005 | 1.004 | 8.761 | 2.086 |
| `sumvm` | 0.996 | 0.996 | 0.058 | 2.085 |
| `nested_loops` | 0.854 | 1.006 | 0.082 | 2.102 |
| `jitloop` | 0.997 | 0.996 | 0.074 | 2.106 |
| `jitkernels` | 0.989 | 0.986 | 0.884 | 2.089 |
| `deltablue` | 0.997 | 0.997 | 19.633 | 2.271 |
| `float_math` | 0.989 | 1.001 | 8.266 | 3.477 |
| `spectral_norm` | 0.997 | 1.002 | 2.526 | 2.095 |
| `json_bench` | 1.005 | 1.001 | 1.501 | 2.744 |
| `str_methods` | 0.997 | 1.001 | 3.137 | 2.175 |
| `dict_ops` | 1.007 | 0.997 | 5.919 | 2.063 |
| `list_ops` | 1.001 | 0.999 | 13.901 | 2.083 |
| `attr_access` | 0.995 | 1.015 | 3.819 | 2.215 |
| `call_overhead` | 1.002 | 1.002 | 8.172 | 2.087 |
| `generators` | 0.993 | 0.994 | 9.934 | 2.101 |
| `deque_ops` | 0.642 | 0.643 | 17.710 | 2.119 |
| `datetime_ops` | 0.524 | 0.501 | 17.649 | 2.244 |
| `pickle_bench` | 1.002 | 1.002 | 343.698 | 2.667 |
| `startup` | 1.012 | 1.018 | 1.470 | 2.093 |

## Supplemental probes

Elapsed times are workload medians in milliseconds. CPU and RSS ratios are medians of paired candidate/reference samples. Workload CPU excludes setup; peak RSS covers the entire process.

| Probe | Baseline ms | New ms | CPython ms | New/base workload CPU | New/base RSS | New/CPython RSS |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `timedelta_integer` | 261.1182 | 66.8506 | 8.8689 | 0.256 | 0.798 | 2.059 |
| `timedelta_float` | 146.7348 | 112.5950 | 4.1444 | 0.766 | 0.794 | 2.063 |
| `timedelta_bigint` | 28.0430 | 21.7624 | 0.8062 | 0.775 | 0.793 | 2.055 |
| `calendar_roundtrip` | 231.2588 | 134.3725 | 2.0881 | 0.584 | 0.795 | 2.056 |
| `deque_iteration` | 103.1099 | 20.6187 | 0.2380 | 0.201 | 0.921 | 2.180 |
| `deque_indexing` | 83.4508 | 18.2566 | 1.0140 | 0.218 | 0.924 | 2.165 |
| `deque_rotate_small` | 115.0114 | 13.1782 | 0.6528 | 0.115 | 0.926 | 2.208 |
| `deque_rotate_large` | 193.9584 | 0.0803 | 0.0077 | 0.000406 | 0.791 | 1.935 |
| `deque_rotate_large_no_prefix` | 188.2789 | 0.2476 | 0.0083 | 0.001306 | 0.816 | 1.995 |

The large rotation probes use 100,000 elements and 200 alternating one-position rotations. `deque_rotate_large` starts after 99 left pops; `deque_rotate_large_no_prefix` includes the initial prefix allocation. These probe-specific speedups should not be generalized to all rotations.

<!-- measurements:end -->

## Changes

Integer `timedelta` components normalize in native code with checked,
128-bit intermediate arithmetic. Large components can cancel before the
final day limit is checked. Floats, integer subclasses, and intermediates
that exceed that width retain Python normalization. The helper validates
exact built-in numeric types once, so those fallback cases avoid repeating
the seven-component validation loop in Python. Native Gregorian
calendar conversions handle exact integers in the public date range;
the Python helpers retain validation and behavior outside that range.

Deque length, truth testing, indexing, rotation, and forward and reverse
iterator advancement run in native code. Rotations transfer the shorter
end of the deque and reuse consumed prefix space. This avoids repeatedly
copying a large container to rotate it by one position. Prefix growth and
compaction are amortized; a first rotation without available prefix space
can still move the backing allocation. Iterators use slots, and their state
check, cursor advance, and item read share the backing list's lock with the
native end operations. Index coercion can run Python callbacks, so it
finishes before borrowing the backing list.

Rotating a deque with at least two elements invalidates iterators even for
zero or a whole turn. Iterator offsets honor `__index__`, clamp negative
offsets to zero, and reject integers outside the index-sized range.

## Measurement

The standard suite keeps all 24 fixtures and their original work sizes.
Each run alternates baseline, candidate, and CPython process order, discards
one warmup cycle, and retains five paired measurement cycles. Both WeavePy
execution modes are measured. The supplemental probes disable the JIT and
also retain five paired cycles. Workload timers exclude imports and input
construction; process elapsed time, CPU time, and peak RSS include them.
The supplemental probes also record workload CPU time.

Both binaries use a writable workspace standard-library cache. Each has a
separate directory selected by its embedded source fingerprint. The
benchmark preflight verifies the full staged runtime before measuring.
Initial development measurements with an unstaged candidate are invalid
and excluded. That setup also caused the datetime suite to enable its
optional timezone stress sweep accidentally; validation was rerun with
the full runtime and the normal resource policy.

Build and reproduce from the repository root:

```sh
cargo build --offline --locked --release -p weavepy-cli --bin weavepy -j 1
export WEAVEPY_STDLIB_CACHE="$PWD/target/performance-stdlib-cache"
python3.14 tools/bench_compare.py \
  --base target/release/weavepy-perf-4177e85 --new target/release/weavepy \
  --samples 5 --out crates/weavepy-bench/census/2026-09-collections-datetime/suite.json
python3.14 crates/weavepy-bench/census/2026-09-collections-datetime/probes.py \
  --base target/release/weavepy-perf-4177e85 --new target/release/weavepy \
  --samples 5 --out crates/weavepy-bench/census/2026-09-collections-datetime/probes.json
```

The baseline executable must be built from the checkpoint commit with the
same release flags. Preserve its executable mode when copying it. Binary
hashes, source hashes, measurements, and validation results are retained in
the [census directory](../crates/weavepy-bench/census/2026-09-collections-datetime/).
The `before-iterator` and `before-validation` subdirectories retain
intermediate candidates; the report uses the final files at the census
directory's root. Run `summarize.py` there to regenerate the tables.

## Validation

All 115 verification entries pass, including the 11 new regression tests in
each of JIT, interpreter, and free-threaded modes; 87 CLI fixtures; CPython's
datetime, deque, collections, and queue suites; and the existing JSON,
string, tracing, generator, exception, and garbage-collection checks.
The free-threaded runtime and queue ping-pong checks also pass. The new
tests cover normalization limits, large cancellations, subclass and float
fallbacks, rotation with and without prefix space, index callbacks,
iterator offsets, shared iterators, and pickling.

At the unchanged full work sizes, the deque and datetime fixtures return
`65011872809` and `369347`, respectively, in CPython, the checkpoint, and
both candidate execution modes. These results are retained separately in
`fixture-results.json`.

Clippy with warnings denied, the VM build without default features, the
benchmark-reporting tests, and the release CLI build pass. Benchmark
preflight was checked against both a valid cache and a deliberately
unusable cache root; the latter is rejected.

The release executable grows from 44,166,432 to 44,184,864 bytes, an increase
of 18,432 bytes (0.042%).

## Compatibility limits

The seeded oracle checks 1,500 timedelta inputs and 500 date arithmetic
cases against CPython in JIT, interpreter, and free-threaded modes. Five
fractional-input results differ from CPython by one microsecond in both
the checkpoint and candidate. These existing differences are retained
explicitly in `oracles.json`; any newly introduced difference fails the
oracle check. Passing these checks doesn't establish complete CPython
compatibility or performance superiority.
