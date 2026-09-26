# Retain receiver paths through eight native attribute reads

The preceding four-field fusion still lost receiver provenance on deeper
chains. An eight-field read compiled with three dynamic attribute lookups,
which repeatedly entered generic lookup and retained owning intermediate pins.
Analysis now remembers seven receiver links, enough to specialize eight reads.
One shared bound controls analysis, lowering, and the runtime helper's input
validation. The compact helper loop and all per-field guards are unchanged.

The eighth read now completes in the same helper as its prefix. Replacing the
fourth intermediate and collecting within the same native activation no longer
keeps that discarded object alive. Deeper reads retain dynamic fallback;
this change doesn't solve arbitrary-length provenance or general pin liveness.

## Measurement

The baseline is the runtime in 91d8422. Test-only commit 02bc968 doesn't change
either measured runtime. The host is macOS x86-64 and the reference is optimized
CPython 3.14.5. Seven alternating paired cycles follow a discarded warmup, with
separate frozen caches. Builds, tests, and diagnostic traces finish before timing.
Every shape performs 1,000,000 reads and verifies its total.

| Eight-field layout | JIT workload/baseline | Elapsed/baseline | CPU/baseline | RSS/baseline | JIT workload/CPython |
| --- | ---: | ---: | ---: | ---: | ---: |
| Dictionary | 0.069 | 0.111 | 0.105 | 0.859 | 0.954 |
| Slots | 0.067 | 0.108 | 0.103 | 0.896 | 1.140 |
| Mixed | 0.055 | 0.087 | 0.083 | 0.861 | 1.042 |

These are medians of paired ratios, with lower values indicating improvement.
The dictionary workload slightly beats CPython, but its process elapsed and
RSS ratios are still 1.088 and 1.463. Slots and mixed storage still exceed
CPython's workload time, elapsed time, CPU, and memory. Four-field dictionary
workload time is 0.990 times baseline. Nine-field reads improve to 0.271 times
baseline but still take 4.836 times CPython's workload time.
Sixteen-field reads improve to 0.737 times baseline but still take 24.764 times
CPython's workload time, exceeding even WeavePy's interpreter-only result of
5.199 times CPython. The deeper dynamic fallback remains a major limitation.

The unchanged 24-fixture suite, over three paired cycles, has these geometric
means. Workload time excludes the startup fixture; process metrics include it.

| Metric | JIT/baseline | Interpreter/baseline | JIT/CPython |
| --- | ---: | ---: | ---: |
| Workload time | 0.989 | 0.996 | 1.077 |
| Process elapsed | 0.981 | 0.987 | 1.320 |
| Process CPU | 0.985 | 0.986 | 1.294 |
| Peak RSS | 1.000 | 0.994 | 1.585 |

The four 31-cycle JIT startup cases have elapsed ratios from 0.985 to 0.996 and
CPU ratios from 0.989 to 0.999. Normal startup still takes 1.371 times CPython's
elapsed time and 1.228 times its peak RSS; imports take 2.630 and 2.248 times,
respectively.

Seven-cycle rechecks reduce the initial Fibonacci interpreter increase from
3.5% to 1.7%; dictionary JIT time and N-body interpreter time improve rather
than reproducing their initial increases. Pi-digits peak RSS remains 5.5%
higher, following 6.7% in the first pass. Its workload time is unchanged.
The apparent 16% sum-loop JIT workload improvement shrinks to 1% on recheck;
interpreter fannkuch remains about 10% faster. Those fixtures don't exercise
deep chains, so these observations don't establish a causal explanation.

The 100-worker probe has workload and RSS ratios of 0.996 and 0.993. Replacing
the fourth node of an eight-field chain, with a 64 KiB payload and explicit
collection every 64 iterations, gives workload, elapsed, CPU, and RSS ratios
of 0.893, 0.897, 0.898, and 0.686 over seven cycles. Its CPython workload and
RSS ratios remain 2.037 and 1.575. The existing four-field replacement probe
initially takes 3.4% more workload time; a seven-cycle recheck reduces that to
1.6%, with 1.6% more peak RSS. These residual costs remain limitations.

## Validation

All 67 JIT crate tests and 368 VM tests pass. The latter pass again after
isolating the worker-lifetime test from unrelated concurrent collections.
Fifty-two release regression scripts pass in JIT, interpreter-only, and
GIL-disabled modes, totaling 156 runs. The new fixture also passes on CPython;
its collection assertion fails on the four-field baseline.

Analyzer tests verify exact specialized/dynamic operation counts at depths
2, 4, 8, 9, and 16, bounded path interning, and convergence when a loop rebinds
its root. Lowering tests verify eight-read fusion, splitting beyond the bound,
and the first-read deoptimization snapshot. Runtime tests verify that six
drivers compile before mutation: dictionary, slots, mixed storage, float,
object, and bytes results. They exercise a descriptor at the sixth node,
late dictionary reordering, missing fields and traceback frames, lane changes,
nullable values, identity, self-cycles, collection, and the observer-mode slot
fallback. A release trace confirms zero dynamic attribute operations in the
eight-field dictionary loop, down from three.

Strict JIT Clippy passes without exclusions. VM Clippy passes with the two
existing category exclusions used in preceding reports. Removing the alignment
exclusion locally reports four unchanged socket address casts on Rust 1.94;
it reports no remaining native-chain cast. The new lowering test uses
thread-local state instead of casting an opaque context pointer.
The runtime helper's opaque context cast carries the same narrowly scoped
alignment allowance as adjacent helpers: its pointer originates from a live,
aligned CallCtx, as required by the native-entry safety contract.

## Reproduction

Build with cargo build --release -p weavepy-cli --bin weavepy and save each
binary before rebuilding. Use tools/bench_compare.py with --samples 7,
--probe tools/bench_attribute_chain_shapes.py, and --work 1000000. Set
WEAVEPY_CHAIN_DEPTH to 2, 3, 4, 8, 9, or 16 and WEAVEPY_CHAIN_LAYOUT to dict,
slots, or mixed. The comparison report records both shape settings. Set
--probe tools/bench_deep_attribute_chain_churn.py --work 16384 to measure
replacement at the former four-field boundary. Set
WEAVEPY_STDLIB_CACHE to a staged standard library and provide a fresh
--frozen-cache-root for each comparison. All generated results and traces
remain under target/. Standard benchmark fixtures and gates are unchanged.
