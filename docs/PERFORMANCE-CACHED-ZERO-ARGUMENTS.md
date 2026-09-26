# Cached zero-argument calls

Zero-argument bytecode calls no longer fetch an empty argument vector from the scratch pool. A cached native bound method borrows its receiver as a one-element slice, matching existing generic bound dispatch, instead of cloning it into a second pooled vector. Calls with arguments retain their existing staging. Cache identity and arity checks, observer fallback, keyword dispatch, exception propagation, receiver reaping, and collection policy are unchanged.

The bound method owns its receiver throughout native execution and reentry. The expanded regression exercises warmed native calls, changing targets, Python methods, errors, expanded arguments, and a destructor that reenters the call site and releases a saved method. Existing coverage checks temporary owners, keyword binding, and descriptor fallback.

## Measurements

The reference is accepted commit `b619ce5df8f91b5e1fabac8a30539aa6ed9252d5`. Comparisons use optimized CPython 3.14.5 on Intel macOS. Process order alternates, warmup is discarded, frozen caches are checked, and setup, assertions, and normal collection remain timed. Builds, tests, and profiling do not overlap timers. The desktop remained active: snapshots include Codex Renderer at 45-52% CPU and Chrome Renderer at 38-44%, so small differences require caution.

Seventeen focused cases have seven initial cycles; fifteen call cases have eleven repeat cycles. Ratios below are repeated candidate/reference measurements; lower is better. Work means the complete probe's internal elapsed interval. CPU and RSS cover the whole process.

| Probe | Work size | JIT work | Interpreter work | JIT CPU | JIT RSS | JIT work / CPython |
|---|---:|---:|---:|---:|---:|---:|
| Saved native length | 100,000 | 0.903 | 0.905 | 0.953 | 1.002 | 4.591 |
| Saved native length | 1,000,000 | 0.925 | 0.903 | 0.933 | 0.999 | 4.663 |
| Saved iterator next | 100,000 | 0.894 | 0.879 | 0.975 | 1.001 | 3.661 |
| Saved weakref call | 100,000 | 0.976 | 0.970 | 0.991 | 0.999 | 8.550 |
| Saved weakref repr | 100,000 | 0.941 | 0.978 | 0.958 | 1.001 | 2.537 |
| Python bound method | 100,000 | 0.986 | 0.991 | 1.003 | 1.005 | 8.558 |

Initial native-length work ratios were 0.905/0.900 at 100,000 calls and 0.880/0.885 at one million calls, for JIT/interpreter mode. Iterator work was 0.900/0.894. The initial Python-method JIT cost of 1.038 did not repeat. The repeated generator-call JIT ratio is 0.970, but its whole-process CPU is 1.000; no CPU improvement is claimed there. Repeated implicit weakref repr retains a 1.023 interpreter work ratio.

The unchanged 24-application suite has three initial cycles. Its geometric means for work/process elapsed/CPU/RSS are 1.005/0.984/0.988/1.003 against the reference in JIT mode, and 1.000/1.000/1.000/0.996 in interpreter mode. Work excludes the startup-only fixture. Against CPython the means are 1.019/1.381/1.241/1.559 for JIT and 2.196/2.000/1.901/1.329 for the interpreter.

Nine selected applications have eleven repeat cycles, including every initial cost above 3% in any measured metric and attribute, call, list, Richards, and DeltaBlue controls. Their reference ratios are 1.006/1.005/1.011/1.004 for JIT and 0.999/0.998/0.997/1.008 for the interpreter. These are selected-repeat means, not a repeated full suite. Attribute work is 1.024/1.025. Initial Fibonacci/float work, datetime CPU, pidigits memory, and DeltaBlue JIT memory costs diminish on repetition.

The repeats retain keyword-call interpreter elapsed at 1.032, Fibonacci JIT CPU at 1.037, and DeltaBlue interpreter RSS at 1.035. Independent 21-cycle diagnostics give 1.001, 0.994, and 1.022, respectively. Keyword JIT CPU is 1.023 in that diagnostic. Every initial, repeat, and diagnostic batch is retained; these costs are not silently discarded.

Two independent 31-cycle startup batches retain small JIT elapsed costs. The repeat ratios are 1.013 for ordinary startup, 1.016 for no-site startup, 1.016 for isolated startup, and 1.008 for imports; CPU ratios are 1.007/1.007/1.013/1.001. The initial no-site CPU ratio was 1.101. Repeated ordinary JIT startup still takes 1.691 times CPython elapsed. No general application, startup, or memory improvement is claimed.

The two large GC controls have seven cycles only: weakref work is 0.995/0.988 and weak-set cleanup is 1.008/1.004. Their JIT work still takes 22.1 and 36.5 times CPython. Their lower measured RSS is not a repeated memory result. CPython superiority remains unachieved.

## Validation and reproducibility

All 409 VM tests, embedding's explicit 1 MiB stack case, 34 C API unit tests, direct weakref C API integration, VM Clippy with established exclusions, no-default compilation, and the release CLI build pass. The frozen binary passes 81 regression runs plus five extra GIL-disabled owner runs, eight trace/profile runs, 36 selected upstream groups across JIT/interpreter/GIL-disabled execution, and twelve native database checks. Upstream groups include function calls, descriptors, GC, all 40 super tests, profiling, monitoring counts, mixed events, and monitored super access. Native extensions can reenable the GIL; separate regressions cover GIL-disabled execution.

Both binaries are 51,894,280 bytes. Reference SHA-256: `ba7f79ba4f96b88333eb6c515036120ce7890d3873ba9953d37e990ce62b0e55`. Candidate SHA-256: `6304bd221247a0d09285ececc59fd94044774709e3fbb28622594ee84571697a`. Runtime patch SHA-256: `e1c578689a83f96a2b3c3e8248e32ba4f1c3b654d79e8beacf90e9f34dad9a40`.

Sources, frozen binaries, all initial/repeated/diagnostic results, validation logs, host snapshots, and source hashes are archived under `target/performance/cached-zero-arguments-investigation/` and its named sibling result files. Reproduce with `tools/bench_compare.py`, `tools/bench_bound_calls.py`, `tools/bench_weakref_methods.py`, and `tools/bench_startup.py`. Benchmark workloads, baselines, thresholds, and JIT budgets are unchanged.

Two preceding weakref dictionary reservation experiments remain held under `target/performance/weakref-dictionary-{build,exact}-investigation/`. They reduced large-population costs but retained unrelated call or startup costs. Neither runtime patch is included here. Their untimed samples motivated this dispatch investigation without establishing the cause of their regressions.
