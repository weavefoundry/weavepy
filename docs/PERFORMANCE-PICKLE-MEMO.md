# Pickle bookkeeping performance

These measurements use the host, release profile, and CPython version recorded
in [Small-call performance](PERFORMANCE-CALL-RETURNS.md). The preceding change
is `92a19cd`; the original baseline is `82f7237`.

The native encoder now uses the runtime's existing fast hasher for its
object-address and class-address maps. Its bounded ancestor stack replaces a
hash set, and strings and bytes skip cycle checks and temporary object clones.
Memo entries still retain their owners, and cyclic or unsupported graphs still
return to the existing pickler. Memo indices and output order don't depend on
hash-table iteration.

Seven interleaved comparisons after a discarded cycle measure the standard
`pickle_bench` fixture at 400 iterations. Ratios below are modified/preceding
release; smaller values indicate less cost.

| Round-trip workload | Default JIT | Interpreter |
| --- | ---: | ---: |
| Workload elapsed time | 0.918 | 0.908 |
| Whole-process elapsed time | 0.939 | 0.932 |
| Whole-process CPU time | 0.936 | 0.933 |
| Peak RSS | 1.008 | 1.007 |

The default-JIT workload still takes 2.675 times CPython's time, with 2.206
times its peak RSS. This is an incremental improvement, not parity. The
executable shrinks by 416 bytes relative to the preceding release.

The supplementary `tools/bench_pickle_graphs.py` probes each perform 2,000
encodings, with an untimed invocation before timing. Seven interleaved cycles
follow a discarded cycle. These test text-heavy graphs, repeated references,
and the longer ancestor scans in a graph with 101 list levels.

| Encoding shape | JIT time | Interpreter time | JIT CPU | JIT RSS | JIT time/CPython |
| --- | ---: | ---: | ---: | ---: | ---: |
| Text | 0.584 | 0.571 | 0.584 | 0.996 | 1.947 |
| Shared children | 0.766 | 0.764 | 0.767 | 1.006 | 5.807 |
| Deep containers | 0.763 | 0.759 | 0.762 | 0.998 | 3.904 |

CPU in this table is the timed workload's CPU consumption. Peak RSS for these
probes is 2.01 to 2.06 times CPython's. All binaries use
distinct writable frozen caches; the comparison verifies that warm cache
contents don't change during timing. Raw results remain in
`target/performance/pickle-direct.json` and `pickle-probes.json`.

## Validation and reproduction

`test_pickle_builtin_data.py`, `test_pickle_native_instances.py`,
`test_pickle_callables.py`, and `test_pure_leaf_returns.py` pass in default JIT,
interpreter-only, and free-threaded modes. The pickle tests cover reference
bytes, shared children, cycles, class hooks, mutations, fallback behavior,
and native-path selection.

```sh
cargo build --release -p weavepy-cli
WEAVEPY_STDLIB_CACHE="$PWD/target/performance/stdlib-cache" \
python3.14 tools/bench_compare.py \
  --base /path/to/original/weavepy --previous /path/to/preceding/weavepy \
  --new target/release/weavepy --fixtures pickle_bench --work 400 --samples 7 \
  --frozen-cache-root target/performance/fresh-pickle-caches \
  --out target/performance/pickle.json

WEAVEPY_PICKLE_SHAPE=deep WEAVEPY_BENCH_WORK=2000 \
  target/release/weavepy tools/bench_pickle_graphs.py
```

For the supplementary warmed comparisons, use `tools.bench_compare.measure`
with `warm=True`, the `tools` directory as `fixture_root`, and separate
`frozen_cache` paths for each binary. Repeat with `WEAVEPY_JIT=0` and CPython.

## Excluded identity experiment

A candidate native identity operation passed 60 JIT Rust tests and 60 Python
regression runs. It reduced a focused identity loop's time by 37.4%, but that
loop's RSS increased by 17.2%, and DeltaBlue slowed by 11.0% relative to the
preceding release. The candidate was removed. Its patch, tests, and raw
measurements are archived under `target/performance/identity-experiment/`
and `target/performance/identity-*.json` rather than included in the change.
