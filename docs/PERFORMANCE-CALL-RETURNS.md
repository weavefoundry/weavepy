# Small-call performance

This pass starts from `82f7237` on macOS x86-64 with Rust 1.94.0 and
CPython 3.14.5. Both WeavePy binaries use the standard release profile and
default features. These measurements don't establish that WeavePy is faster
than CPython across every meaningful metric.

The installed CPython is configured with PGO, LTO, and the tail-call
interpreter. Its GIL is enabled, and its experimental JIT isn't available.

The initial three-sample census includes all 24 standard fixtures, including
the library census. Over the 23 timed workloads, WeavePy's geometric mean is
1.083 times CPython's time, with eight wins. Across all 24 processes, peak RSS
is 1.710 times CPython's. DeltaBlue has the largest workload-time gap, at
7.19 times CPython's time. Profiling it identifies interpreter dispatch,
small leaf calls, and attribute resolution as substantial costs.

## Direct returns

The pure-leaf classifier now recognizes functions that return an argument,
constant, small integer, or an argument's attribute. These shapes reuse the
existing classification byte and avoid setting up the general evaluator's
operand stack and owned-value scratch. The existing binding, recursion,
observer, class-version, and attribute-name checks still apply. Other shapes
retain the general evaluator.

Seven interleaved cycles, after a discarded cycle and an untimed invocation
per process, measure `tools/bench_leaf_returns.py` at 500,000 iterations.
Ratios below are modified/baseline, with smaller values indicating less cost.

| Focused return workload | Default JIT | Interpreter |
| --- | ---: | ---: |
| Workload elapsed time | 0.874 | 0.904 |
| Workload CPU time | 0.876 | 0.904 |
| Whole-process CPU time | 0.899 | 0.913 |
| Peak RSS | 1.004 | 1.001 |

The complete standard suite, with three measured cycles and distinct warmed
frozen caches per binary, is essentially unchanged: workload-time geometric
means are 0.998 with the JIT and 1.001 without it. Whole-process CPU ratios are
1.004 and 0.999, and peak-RSS ratios are 1.007 and 1.003. The executable grows
by eight bytes, from 51,830,424 to 51,830,432 bytes.

This is a focused call-throughput improvement, not a demonstrated DeltaBlue
improvement. DeltaBlue's time ratios are 1.007 with the JIT and 1.026 without
it in the complete suite; an earlier seven-cycle comparison gives 0.990 and
1.029. The suite's 12.2% increase in `list_ops` peak RSS doesn't reproduce in
a seven-cycle recheck, which measures a 1.3% increase and essentially unchanged
execution time. All samples, including unfavorable results, remain under
`target/performance/`.

## Validation and reproduction

Eight regression scripts pass in default JIT, interpreter-only, and
free-threaded modes, for 24 runs. They cover direct returns, call binding,
scalar results, native calls, closures, and core-loop iteration. The new
`test_pure_leaf_returns.py` also passes on CPython and the baseline. It checks
descriptor and class mutations, instance dictionary reshaping, missing
attributes, ownership, changed defaults and code, and tracing.

```sh
cargo build --release -p weavepy-cli
# Save a separate baseline binary before making changes.
WEAVEPY_STDLIB_CACHE="$PWD/target/performance/stdlib-cache" \
python3.14 tools/bench_compare.py \
  --base /path/to/baseline/weavepy --new target/release/weavepy \
  --samples 3 --frozen-cache-root target/performance/new-comparison-caches \
  --out target/performance/suite.json

WEAVEPY_BENCH_WORK=500000 target/release/weavepy tools/bench_leaf_returns.py
WEAVEPY_BENCH_WORK=500000 WEAVEPY_JIT=0 \
  target/release/weavepy tools/bench_leaf_returns.py
WEAVEPY_BENCH_WORK=500000 python3.14 tools/bench_leaf_returns.py
```

Run timing comparisons after compilation and other tests have finished. Keep
launch conditions consistent and compare matched cycles. The focused warmed
measurements use `tools.bench_compare.measure` with `warm=True` and
`fixture_root` set to the `tools` directory. Raw timing data and profiles belong
under `target/`, not in Git. No CI baselines or thresholds were changed.
