# JIT attribute insertion performance

Compiled attribute stores now retain the name's Python hash and reuse the
guard's interned string. A raw dictionary entry serves both insertion and
replacement, avoiding a second lookup. A probe that encounters a key requiring
Python equality resumes in the interpreter before storing. Class-version,
watcher, displaced-value, and deferred-tracking checks remain in place.

## Measurement

The preceding executable is `76ff1a0` (the same runtime as `31b6d4d`). Both
releases use the same release profile on the Intel macOS host and compare with
CPython 3.14.5. Seven interleaved measured cycles follow a discarded cycle,
using separate frozen caches verified unchanged during measurement. Standard
fixtures retain their default work sizes. Ratios are medians of paired samples;
smaller values mean less cost.

| Default JIT, modified/preceding | Workload time | Process CPU | Peak RSS |
| --- | ---: | ---: | ---: |
| `float_math` | 0.971 | 0.976 | 1.002 |
| `attr_access` | 0.993 | 0.999 | 1.018 |
| `call_overhead` | 0.997 | 0.992 | 1.000 |
| `deltablue` | 0.985 | 0.991 | 1.004 |
| `startup` | 1.008 | 1.002 | 1.007 |

The separate warmed `float_math` run has a 0.944 elapsed-time ratio,
0.944 workload-CPU ratio, and 1.023 peak-RSS ratio. Its
workload time remains 2.478 times CPython's, and its peak RSS remains 2.553
times CPython's. This is an execution-time improvement, not memory parity.

A three-cycle comparison of all 24 standard fixtures has default-JIT geometric
means of 0.990 for workload time (excluding startup), 0.993 for process elapsed
time, 0.993 for process CPU, and 1.002 for peak RSS. Interpreter ratios are
1.005, 0.996, 0.994, and 1.000, respectively. Against CPython, the default-JIT
ratios are 1.075, 1.388, 1.381, and 1.735.

Seven-cycle rechecks reduce the initial interpreter increases for `fib` and
`str_methods` to 1.2% and 1.5%. The default-JIT `deque_ops` slowdown remains
3.8% (the initial three-cycle estimate was 5.4%); it is a retained tradeoff.

## Validation

The CLI release build and targeted VM Clippy pass. Clippy uses the same two
existing lint-category exclusions documented in the preceding report. Seven
regression scripts pass in default-JIT, interpreter-only, and free-threaded
modes (21 runs). The new attribute-insertion test also passes on CPython; its
JIT trace confirms compilation of the constructor and caller. It covers
insertion order, dictionary exports, reinserting deleted keys, overwritten
finalizers and weakrefs, equality callbacks, and descriptor invalidation.

Broader probes also found preexisting differences: assigning `__dict__` copies
the supplied mapping, and a raising non-string dictionary key can lose its
comparison exception. Both occur in the preceding release, including with the
JIT disabled. Their reproducers and logs are retained under `target/performance/`;
this optimization does not claim to fix them.

```sh
cargo build --release -p weavepy-cli --bin weavepy
WEAVEPY_STDLIB_CACHE="$PWD/target/performance/stdlib-cache" \
python3.14 tools/bench_compare.py \
  --base /path/to/preceding/weavepy --new target/release/weavepy \
  --fixtures float_math attr_access call_overhead deltablue startup \
  --samples 7 --frozen-cache-root target/performance/fresh-attribute-caches \
  --out target/performance/attributes.json
```

Raw samples and logs are under `target/performance/attr-store-*`. Benchmark
baselines, workload definitions, and gate thresholds are unchanged.
