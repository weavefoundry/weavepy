# Release retired JIT code metadata

Return-type memoization held a strong code reference alongside the tier cache.
Each cache waited until it was the sole owner before eviction, so neither could
release the code. Return-type entries now use weak handles. Their allocation
addresses remain reserved, keeping pointer keys safe while code payloads can
be released.

Tier-cache eviction also discounts the weak-reference registry's temporary
strong clones, as the cycle collector already does. Python weak references
therefore no longer prevent eviction. Live functions and native dependencies
still count as owners. Native executable pages retain their existing lifetime.
Recursive callee metadata can still retain itself; this change does not resolve
that separate ownership cycle.

## Measurement

The baseline is `82e20ab`. The macOS x86-64 host, CPython 3.14.5 reference,
release profile, alternating paired samples, and isolated frozen caches match
the [startup report](PERFORMANCE-STARTUP-WORK.md). Builds and tests finish
before measurement, and frozen-cache contents stay unchanged during sampling.

`tools/bench_jit_code_churn.py` creates, runs, and retires 100 generated modules.
Each module has two functions and 64 KiB of private lookup data. Four weak
references per module track its functions and code objects, as a metadata
monitor might. Three explicit collections per module allow tier-cache and
cycle-collector releases to settle. The probe checks its calculated result.

Seven paired cycles produce these modified/baseline ratios:

| Default JIT, watched code churn | Ratio |
| --- | ---: |
| Workload time | 1.080 |
| Process elapsed | 1.061 |
| Process CPU | 1.058 |
| Peak RSS | 0.760 |

Marginal median peak RSS falls from 27.56 MB to 21.02 MB. Releasing the retained
metadata and module data costs execution time in this collection-heavy probe.
The modified runtime still takes 2.460 times CPython's workload time and 1.769
times its peak RSS. This is a focused lifetime and memory improvement.

The three-cycle full suite gives these geometric means. Workload time covers
23 fixtures; process metrics cover all 24, including startup.

| Metric | JIT/baseline | Interpreter/baseline | JIT/CPython |
| --- | ---: | ---: | ---: |
| Workload time | 0.995 | 1.003 | 1.071 |
| Process elapsed | 0.990 | 0.992 | 1.284 |
| Process CPU | 0.994 | 0.993 | 1.276 |
| Peak RSS | 1.004 | 0.994 | 1.620 |

Seven-cycle rechecks remove the initial pi-digits memory increase, JIT-kernel
and attribute-access JIT slowdowns, and datetime interpreter slowdown. JSON
retains 3.2% higher JIT RSS; its recheck also shows 3.7% higher workload time
and 1.0% higher process CPU. Interpreter-only attribute access is 2.9% slower.
Fannkuch's initial interpreter RSS increase also disappears on recheck.

A separate ordinary-churn probe keeps no weak metadata watchers and performs
one collection per module. Its initial three-cycle slowdown doesn't repeat:
seven-cycle JIT ratios are 1.001 for workload time and 1.014 for RSS. That case
therefore establishes no material memory or throughput gain. The substantial
memory reduction above specifically requires the retained weak watchers.

## Validation

The regression creates compiled caller/callee pairs, checks that live code
survives collection, then retires both functions. All four function/code weak
references must die after three collections, and each callback must run exactly
once. It passes on CPython and fails on the preceding WeavePy release. The
forced-JIT Rust wrapper and all 152 JIT unit tests pass. The 34 selected Python
regression scripts pass in JIT, interpreter-only, and free-threaded modes
(102 runs), covering native calls, suspended generators, pin retirement,
weakrefs, finalization, shared code, and ordinary language behavior.

The release CLI build, formatting, and targeted Clippy pass with the preceding
reports' two local lint-category exclusions. Traces confirm that both probe
functions compile. A separate recursive diagnostic confirms that the caller
is released but recursive callee metadata still survives collection.

```sh
WEAVEPY_STDLIB_CACHE="$PWD/target/performance/stdlib-cache" \
WEAVEPY_BENCH_LAUNCH_CONTEXT=sandboxed \
python3.14 tools/bench_compare.py \
  --base /path/to/82e20ab/weavepy --new target/release/weavepy \
  --probe tools/bench_jit_code_churn.py --work 100 --samples 7 \
  --frozen-cache-root target/performance/fresh-code-churn-caches \
  --out target/performance/code-churn.json
```

Raw samples and diagnostics remain under `target/performance/weak-return-*`
and `target/performance/jit-cache-experiment/`. Standard fixtures, work sizes,
CI baselines, and gate thresholds are unchanged.
