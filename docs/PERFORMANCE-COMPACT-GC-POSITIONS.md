# Compact collector position hints

The collector's two cached vector positions now use four-byte atomic hints. A handle occupies 64 bytes instead of 72 on this 64-bit release. Its reference counters, identity, marking flags, finalizer state, collection thresholds, and scan budgets are unchanged. Positions beyond the cacheable range use the existing identity-checked pointer-search fallback. Distinct absent and uncached markers preserve finalizer hot-set membership. There is no new heap limit or allocation.

Release-linked layout and LLVM diagnostics confirm an 80-byte Arc allocation request instead of 88. This host allocator rounds those requests to 80 and 96 bytes. Whole-process savings depend on the rest of the workload.

## Measurements

Accepted `1ef7a98`, optimized CPython 3.14.5, and the candidate were measured on Intel macOS using alternating paired runs, discarded warmup, ordinary release settings, and verified frozen caches. Builds, tests, and profiles ended before timing. Background media analysis and indexing were recorded; original and repeated results are retained. The subsequent `9a12239` change only repairs test assertion spelling.

Seven-cycle ordinary-population runs cover zero, 10,000, and 100,000 objects in nine modes. The new GC-population probe covers retained owners, promotion, release, self-cycles, finalizers, weak references, callbacks, and freeze/unfreeze. Its timer includes collection and lifetime assertions. It passes 100 semantic checks across CPython, the accepted release, and all candidate execution modes; these validation timings are excluded.

Ratios are candidate/accepted; lower is better. At 100,000 objects:

| Population | JIT work time | Interpreter work time | JIT peak RSS | Interpreter peak RSS |
| --- | ---: | ---: | ---: | ---: |
| Integer subclass | 0.979 | 0.946 | 0.971 | 0.953 |
| String subclass | 0.956 | 1.013 | 0.977 | 0.958 |
| Tuple subclass | 0.981 | 1.013 | 0.982 | 0.981 |
| Self-cycles with weak references | 0.980 | 0.977 | 1.001 | 0.993 |
| Finalizers with weak references | 0.989 | 0.994 | 0.978 | 0.990 |
| Weak references | 0.977 | 0.975 | 1.025 | 1.013 |
| Weak-reference callbacks | 1.007 | 0.992 | 0.991 | 1.008 |
| Frozen self-cycles | 0.987 | 1.002 | 0.996 | 0.988 |

Eleven-cycle repeats at 300,000 objects confirm integer-subclass JIT/interpreter
RSS ratios of 0.972/0.951 and string-subclass ratios of 0.970/0.969. Their work
time ratios are 0.992/1.007 and 0.989/0.981. The smaller handle produces a
consistent memory benefit in these workloads; GC-population RSS remains mixed.

The unchanged 24-fixture suite, three cycles, gives the following geometric means. Work time excludes the startup fixture; process metrics include it.

| Metric | JIT/accepted | Interpreter/accepted | JIT/CPython |
| --- | ---: | ---: | ---: |
| Work time | 1.007 | 0.979 | 1.037 |
| Process elapsed | 1.007 | 0.984 | 1.400 |
| Process CPU | 1.018 | 0.988 | 1.265 |
| Peak RSS | 1.002 | 0.992 | 1.554 |

Independent eleven-cycle suite repeats don't reproduce the initial dictionary,
string, or n-body timing increases. Warmed dictionary and string tests at
500,000 work record JIT/interpreter work ratios of 0.988/0.981 and 0.981/0.991.
The standard JSON repeat still records 1.034 JIT time and the deque repeat
1.031, but seven-cycle warmed checks at 1,000 JSON and 1,000,000 deque work
record 0.989/0.978 and 1.017/1.007. Workload CPU confirms those warmed ratios.
The first list-operation repeat also records 1.029 JIT RSS, versus 0.990 in
the original suite. Both results are retained.

Thirty-one-cycle startup comparisons give normal/no-site/isolated/import JIT elapsed ratios of 1.024/1.016/1.020/0.991. Normal-startup CPU is 1.026 and RSS is 1.003. An independent thirty-one-cycle repeat gives 1.005/1.024/1.011/0.999 JIT elapsed
ratios. Normal/no-site/isolated CPU ratios remain 1.015/1.045/1.021, with
no-site RSS at 1.015. These startup costs and the sustained deque cost remain
optimization targets. This revision is retained for its consistent 3-5% memory
reduction in larger native-subclass populations, with approximately unchanged
broad JIT work time.

Native subclass work remains many times slower than CPython; the 100,000-callback probe takes about 81 times CPython's work time. This change does not establish CPython parity.

## Validation and reproduction

The candidate passes 398 VM tests, including numeric hint boundaries, forced uncached and stale positions, frozen and generation removal, finalizer hot/cold membership, and moved-position updates. Embedding retains its 1 MiB stack test. VM Clippy with established exclusions, no-default compilation, formatting, and all fourteen benchmark-tool tests pass. The frozen CLI passes 315 runs across 105 regression fixtures, 1,048 semantic probes, eighteen CPython fixtures, 180 ordinary-population checks, and 100 GC-population checks. All 53 probe, three application, and nine population compilation decisions remain unchanged.

```sh
cargo build --release -p weavepy-cli --bin weavepy
WEAVEPY_INSTANCE_KIND=gc-callback \
WEAVEPY_STDLIB_CACHE="$PWD/target/performance/stdlib-cache" \
python3.14 -B tools/bench_compare.py \
  --base /path/to/1ef7a98/weavepy --new target/release/weavepy \
  --probe tools/bench_gc_populations.py --work 100000 --samples 7 \
  --frozen-cache-root target/performance/fresh-gc-population-caches \
  --out target/performance/gc-population-comparison.json
```

The frozen CLI is 51,881,744 bytes, SHA-256
`cf0e2ad407d900cea3091b0df0c75458c6918ed5b42438c3d23ac9499892fb9d`.
The runtime patch against `1ef7a98` is
`7f7d6269934ddabf726bd0f136d56c24cf30d946beaa2117f8a777a7d8548494`.
Raw diagnostics, source snapshots, validation, and population samples remain
under `target/performance/compact-gc-positions-investigation/`. Full-suite and
startup samples use `target/performance/compact-gc-positions-r1ef7a98-*`.

A later exploratory mixed-target fixture exposes an existing lifetime gap in
both accepted and candidate releases: class and bound-method populations may
need a second explicit collection where CPython clears them in one. The
reproducer and baseline diagnostics are retained under
`target/performance/weakref-wrapper-allocation-investigation/`; it isn't counted
among passing tests. An initial assertion that every dead reference clears its
callback property was also rejected by CPython itself and corrected in the
research fixture.
