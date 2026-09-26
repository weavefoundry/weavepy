# Borrow cached attribute chains

Long ordinary-instance attribute chains now share a borrowed native walk
across the eight-field guarded boundary. The helper reads up to 32 consecutive
fields, retains only the final result, and can feed an adjacent existing
exact-integer guard directly. Previously, each dynamic suffix read created
another owning pin and crossed another helper boundary.

Each dynamic read validates its bytecode cache, class version, field name and
index, and ordinary instance kind. The walk runs no Python and retains no
borrow across a callback. Missing or changed fields, unsupported receivers,
observers, mutable borrows, and free threading retain the original paths.
A suffix miss reuses the completed guarded prefix with its existing pin-reuse
policy, then runs the original suffix helpers at their exact bytecode positions.
Type inference, method boundaries, pin limits, and retirement thresholds are
unchanged.

## Measurements

The baseline is the preserved e51dd38 release, compared with this candidate
and optimized CPython 3.14.5 on macOS x86-64. CPython's experimental JIT is
disabled. Paired process order alternates, one warmup cycle is discarded, and
frozen caches are separate per binary and verified unchanged during timing.
Builds, tests, and profiles finish before benchmarks begin. Ratios are paired
medians; lower is better. Workload timers exclude process startup.

Seven cycles per ordinary shape execute one million iterations, including
chain construction inside the timer.

| Chain | JIT work/base | JIT RSS/base | Interpreter work/base | JIT work/CPython |
| --- | ---: | ---: | ---: | ---: |
| dict-4 | 1.021 | 1.009 | 1.048 | 0.742 |
| dict-8 | 1.036 | 1.006 | 1.004 | 0.879 |
| dict-9 | 0.890 | 0.953 | 1.028 | 1.324 |
| dict-16 | 0.358 | 0.897 | 0.997 | 1.764 |
| slots-16 | 0.303 | 0.888 | 0.984 | 1.895 |
| mixed-16 | 0.335 | 0.885 | 0.998 | 2.022 |
| dict-17 | 0.336 | 0.880 | 0.997 | 1.860 |
| dict-32 | 0.275 | 0.670 | 0.996 | 2.417 |
| dict-33 | 0.308 | 0.753 | 1.002 | 2.816 |

Independent seven-cycle short-chain rechecks give dict-4 JIT ratios of 1.008
cold and 1.018 warm; its interpreter increase disappears. Dict-8 costs persist
at 1.056 cold and 1.038 warm, with interpreter ratios of 0.992 and 1.004.
These short shapes don't use the mixed-chain helper. Longer chains still
trail CPython despite the large gains.

Sixteen-read fallback probes place a class, imported module, or simple property
after eight links. The completed guarded prefix avoids a repeated walk, but
speculation can still repeat dynamic reads before the unsupported link.

| Fallback | Cold JIT/base | Warm JIT/base | Independent cold recheck |
| --- | ---: | ---: | ---: |
| class | 1.039 | 1.022 | 1.052 |
| module | 1.059 | 1.009 | 1.036 |
| property | 1.045 | 1.025 | 1.041 |

Initial runs use seven cycles; cold rechecks use eleven. Class-fallback peak
RSS repeats at 1.040 times baseline. Seven-cycle separate attribute-read
probes span 0.938-1.026 JIT workload ratios. Their expressions aren't fused
chains, so those results don't establish a fusion gain.

## Applications and process costs

The unchanged 24-fixture suite runs three paired cycles. Workload geometric
means cover 23 timed workloads; process metrics include startup as well.

| Metric | JIT/base | Interpreter/base | JIT/CPython |
| --- | ---: | ---: | ---: |
| Workload time | 0.991 | 1.005 | 1.087 |
| Process elapsed | 1.003 | 1.005 | 1.348 |
| Process CPU | 1.007 | 1.005 | 1.299 |
| Peak RSS | 1.006 | 0.994 | 1.588 |

All ten fixtures with an initial increase above 3% in either mode or any
recorded metric received seven-cycle rechecks. Fibonacci and deque changed
direction, while interpreter fannkuch/sumvm and JIT pickle remained slower.
Eleven additional cycles use about ten times the loop work for those five
cases. Fibonacci grows from 27 to 32 because its call count is exponential.
These supplemental runs don't replace the unchanged suite's aggregate.

| Longer workload | JIT work/base | Interpreter work/base | JIT RSS/base |
| --- | ---: | ---: | ---: |
| fannkuch | 0.978 | 1.001 | 1.015 |
| sumvm | 1.000 | 1.021 | 1.008 |
| fib | 0.997 | 1.007 | 1.006 |
| deque_ops | 1.007 | 0.992 | 1.001 |
| pickle_bench | 1.032 | 1.025 | 1.014 |

The existing fannkuch fixture repeats sorted-list construction and a call;
its flip loop never executes. These aren't canonical fannkuch-redux results.
Pickle's initial JIT ratio is 1.057, its seven-cycle ratio is 1.115, and its
larger-work ratio is 1.032. Twenty-one paired instrumented traces find the
same seven compilation attempts inside its timer, with a 0.978 total compile
ratio. Separate owned-process samples show cloning, allocation, and native
pickle parsing, with no samples in the mixed-chain helper. Neither diagnostic
explains the remaining slowdown, and neither replaces the timing results.

Seven-cycle replacement probes include allocation and collection. Four-,
eight-, and sixteen-field JIT workload ratios are 1.011, 0.963, and 0.904;
peak RSS ratios are 0.967, 1.008, and 0.737. The sixteen-field case still takes
2.136 times CPython's workload time and 1.616 times its RSS. The 100-worker
probe gives JIT workload/RSS ratios of 1.004/1.024, versus 4.693/1.947 against
CPython.

Two independent 31-cycle startup sweeps give the following second-run results.
The first normal elapsed/RSS ratios were 1.029/1.013. The initial no-site CPU
increase of 9.4% shrinks to 2.5% in the recheck.

| Startup | JIT elapsed/base | JIT RSS/base | JIT elapsed/CPython | JIT RSS/CPython |
| --- | ---: | ---: | ---: | ---: |
| normal | 1.020 | 1.009 | 1.474 | 1.232 |
| no site | 1.018 | 1.016 | 0.829 | 0.758 |
| isolated | 1.023 | 1.012 | 1.479 | 1.235 |
| imports | 1.006 | 1.003 | 2.747 | 2.231 |

This is a focused throughput and retention improvement with remaining costs.
It doesn't establish superiority over CPython across workloads or metrics.

## Validation and reproduction

All 69 JIT tests, 373 VM tests, and 174 selected release regression runs pass.
Release regressions cover JIT, interpreter-only, and GIL-disabled modes.
The isolated lowering test exercises full hits, untouched misses, completed
prefixes, exact exception/deopt/park positions, and method boundaries. The VM
test proves native object/integer/prefix hits before testing changed classes,
fields, descriptors, callbacks and frames, nullable/scalar results, identity,
collection, suspended generators, and observers.

A separate cold-finalizer diagnostic still fails on both e51 and this
candidate: a cold unsupported suffix can leave an intermediate pin live until
native exit. Warmed replacement cases pass. General fallback pin liveness
isn't fixed by this change.

Formatting, strict JIT Clippy, VM Clippy with the existing local
`let_and_return` and `cast_ptr_alignment` exemptions, and six benchmark-tool
tests pass. The release executable is 51,814,592 bytes, 19,336 bytes larger
than e51. Its SHA-256 is
`feece89069eb93eaef001302a212c9243b68872e65af57c6b26a8073a8b6a69a`.
Raw measurements, exact measured probe sources, profiles, and build provenance
remain under `target/performance/cached-prefix-*` and
`target/performance/cached-chain-prefix-experiment/`.

```sh
cargo build --release -p weavepy-cli --bin weavepy
WEAVEPY_STDLIB_CACHE="$PWD/target/performance/stdlib-cache" \
WEAVEPY_BENCH_LAUNCH_CONTEXT=sandboxed WEAVEPY_CHAIN_DEPTH=16 \
python3.14 tools/bench_compare.py \
  --base /path/to/e51dd38/weavepy --new target/release/weavepy \
  --samples 7 --probe tools/bench_attribute_chain_shapes.py --work 1000000 \
  --frozen-cache-root target/performance/fresh-chain-caches \
  --out target/performance/chain-comparison.json
```

Use `--warm` for a separate steady-state check. Use the fallback probe with
`WEAVEPY_CHAIN_FALLBACK=class`, `module`, or `property`. Standard fixtures,
work sizes, CI baselines, and thresholds are unchanged.
