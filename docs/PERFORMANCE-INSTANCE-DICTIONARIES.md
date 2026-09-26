# Existing instance dictionary access

`PyInstance::dict_cell` now obtains an already published dictionary before
reading allocation-only metadata. Class borrowing, class reference counting,
and the capacity hint run inside the lazy initializer. The common path is the
same acquiring pointer load that `LazyArc` already uses. Publication and the
deferred-owner write barrier are unchanged.

## Measurement

The immediate baseline is `3afe723`. The additional baseline is `76ff1a0`,
before the attribute-store and dictionary-access changes. Seven paired measured
cycles follow a discarded cycle, using the existing fixtures and work sizes,
separate frozen caches, the same release profile, and the Intel macOS host
described in [the attribute-store report](PERFORMANCE-ATTRIBUTE-STORES.md).

| Modified/immediate baseline | JIT time | Interpreter time | JIT CPU | JIT RSS |
| --- | ---: | ---: | ---: | ---: |
| `float_math` | 0.956 | 1.014 | 0.960 | 1.002 |
| `attr_access` | 0.946 | 0.876 | 0.961 | 1.015 |
| `call_overhead` | 0.967 | 0.949 | 0.978 | 1.012 |
| `deltablue` | 0.986 | 0.989 | 0.986 | 0.997 |
| `deque_ops` | 0.956 | 0.953 | 0.966 | 1.000 |
| `startup` | 1.011 | 1.001 | 1.011 | 1.016 |

Against `76ff1a0`, the combined default-JIT time ratios in this same run are
0.904 for `float_math`, 0.959 for `attr_access`, 0.979 for `call_overhead`,
0.985 for `deltablue`, 1.014 for `deque_ops`, and 0.989 for startup. These
are directly paired measurements, not products of earlier improvements.

The three-cycle full suite has the following geometric means. Workload time
covers 23 workloads; the process metrics cover all 24 fixtures, including
startup. Smaller ratios mean less cost.

| Metric | JIT/baseline | Interpreter/baseline | JIT/CPython |
| --- | ---: | ---: | ---: |
| Workload time | 0.975 | 0.988 | 1.067 |
| Process elapsed time | 0.981 | 0.986 | 1.373 |
| Process CPU | 0.986 | 0.985 | 1.374 |
| Peak RSS | 0.996 | 0.997 | 1.728 |

The initial 5.2% n-body peak-RSS increase does not repeat: seven additional
cycles give a 0.990 JIT RSS ratio and a 0.985 workload-time ratio.

## Validation

The release CLI build, formatting, and targeted VM Clippy pass. The two
preexisting Clippy categories excluded locally are unchanged from the preceding
report. Eleven regression scripts pass in default-JIT, interpreter-only, and
free-threaded modes (33 runs), including lazy dictionary publication, exported
dictionary ownership, class reassignment, concurrent writes, cycle collection,
finalization, native attribute insertion, and JIT calls.

```sh
cargo build --release -p weavepy-cli --bin weavepy
WEAVEPY_STDLIB_CACHE="$PWD/target/performance/stdlib-cache" \
python3.14 tools/bench_compare.py \
  --base /path/to/3afe723/weavepy --new target/release/weavepy \
  --samples 3 --frozen-cache-root target/performance/fresh-dict-caches \
  --out target/performance/dictionaries.json
```

Raw samples and logs remain under `target/performance/dict-cell-*`. CPython
parity remains unmet, particularly for object-heavy execution and process
memory. Standard fixtures, CI baselines, and gate thresholds are unchanged.
