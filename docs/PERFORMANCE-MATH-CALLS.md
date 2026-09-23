# Guarded native math calls

Native math guards retain the module dictionary, exact string key, cached hash,
and checked entry index. Every entry validates the function identity. A deleted
or moved key triggers a callback-free lookup; a changed function or module
resumes ordinary execution. The module itself remains protected by the global
identity guard.

Bounded scalar leaves can now use guarded scalar globals and the context-free
`sqrt`, `fabs`, `sin`, and `cos` intrinsics. Their native callers validate guards
before using the existing lightweight scalar-call entry. A numeric deoptimization
restarts the pure body in an ordinary frame, preserving exceptions and tracebacks.

## Measurements

The baseline is `30114fb`. Both binaries use the same release profile on the
Intel macOS host with CPython 3.14.5, as described in
[the attribute-store report](PERFORMANCE-ATTRIBUTE-STORES.md). Seven measured
cycles alternate process order after a discarded cycle. Frozen caches are
separate by binary and verified unchanged. Ratios are medians of paired samples;
smaller values mean less cost.

| Default JIT, modified/baseline | Workload time | Process CPU | Peak RSS |
| --- | ---: | ---: | ---: |
| `float_math` | 0.917 | 0.924 | 0.990 |
| `attr_access` | 1.003 | 1.002 | 1.011 |
| `call_overhead` | 0.999 | 0.995 | 1.000 |
| `deltablue` | 0.990 | 0.991 | 1.021 |
| `nbody` | 0.987 | 0.998 | 1.004 |

The supplemental `tools/bench_math_calls.py` probe calls a function containing
`sin` and `cos` 500,000 times. After one workload warmup, its default-JIT ratios
are 0.342 for workload time and CPU, 0.471 for process elapsed time, 0.462 for
process CPU, and 0.999 for peak RSS. Against CPython, those ratios are 0.439,
0.440, 0.585, 0.589, and 1.536, respectively. The speedup does not establish
memory parity. A second seven-cycle run gives a 0.362 JIT time ratio and 0.436 versus
CPython. The interpreter-only probe is 3.9% slower in the first run and 5.1%
slower in the recheck, a retained regression despite its unchanged execution path.

A three-cycle run of all 24 standard fixtures gives these geometric means.
Workload time excludes startup; process metrics include it.

| Metric | JIT/baseline | Interpreter/baseline | JIT/CPython |
| --- | ---: | ---: | ---: |
| Workload time | 0.996 | 0.984 | 1.060 |
| Process elapsed time | 0.996 | 0.982 | 1.372 |
| Process CPU | 0.998 | 0.982 | 1.367 |
| Peak RSS | 0.993 | 0.992 | 1.725 |

Seven-cycle rechecks reduce the initial call-overhead time increase to 0.3%
and the pidigits RSS ratio to 0.994. The string-method workload remains 3.8%
slower (3.7% more process CPU); that tradeoff is retained and reported.

CPython parity remains unmet. DeltaBlue is still 6.928 times CPython's
workload time in this run.

## Validation

The release CLI build, four JIT engine unit tests, formatting, and targeted
VM/JIT Clippy pass. Clippy retains the two existing local lint-category
exclusions described in the preceding reports. Seventeen regression scripts
pass in default-JIT, interpreter-only, and free-threaded modes (51 runs).
The new math guard test also passes on CPython. Its warmed trace confirms
2,452 scalar-leaf calls, including calls through compiled callers.

Coverage includes moved and deleted dictionary keys, replaced math functions,
rebound globals, shared code with separate globals, mutation in a nested
callback, negative zero, NaN, domain errors and traceback frames, and profiling.
The engine tests enter each admitted math intrinsic without a helper context
and verify numeric deoptimization. Existing native-call, object-lane, lifetime,
attribute, dictionary-publication, and GC regressions also pass.

The comparison tool now accepts a standalone `--probe` with an explicit work
size and records the source hash. Its six reporting tests pass.

```sh
cargo build --release -p weavepy-cli --bin weavepy
WEAVEPY_STDLIB_CACHE="$PWD/target/performance/stdlib-cache" \
python3.14 tools/bench_compare.py \
  --base /path/to/30114fb/weavepy --new target/release/weavepy \
  --probe tools/bench_math_calls.py --work 500000 --warm --samples 7 \
  --frozen-cache-root target/performance/fresh-math-caches \
  --out target/performance/math-calls.json
```

Raw results remain under `target/performance/math-call*`. Standard fixtures,
work sizes, CI baselines, and gate thresholds are unchanged.
