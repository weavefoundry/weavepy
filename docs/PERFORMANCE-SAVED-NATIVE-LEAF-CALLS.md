# Saved native leaf calls

Saved zero-argument native methods can now execute in the quiet leaf loop. Exact list, dictionary, set, and string lookup reuses native bodies from the existing leaf table, giving saved methods a stable identity. Supported calls include length, list copy/reverse, dictionary views, and selected string operations. Dispatch checks the actual callable identity and receiver shape; a matching name alone isn't sufficient.

A method must have another program owner before its operand can leave this path. The existing prompt-reap predicate discounts collector handles and weakref-registry clones. Potentially releasing mutations, arbitrary registered native bodies, descriptor redispatch, and unmatched shapes retain full dispatch. Hooks and GIL-disabled execution retain ordinary dispatch. Rejected entries weakly reference the bound method and check the registry generation; the core loop sends a matching rejection directly to full dispatch. Python functions are rejected before native-cache access. Neither cache keeps a saved method or its receiver alive.

## Measurements

The reference is accepted commit `e389998781c5bd08c99bf392e200fe15721ff0ee`, compared with optimized CPython 3.14.5 on Intel macOS. Process order alternates, warmup is discarded, frozen caches are verified, and setup, assertions, and normal collection remain timed. Owned builds, tests, and profiles don't overlap timings. The desktop remained active: snapshots include Codex Renderer at 34% before initial timing and 90% before diagnostics. Small differences require caution.

Twenty-two focused cases have seven initial cycles; twenty call cases have eleven repeats. Each focused cycle includes the accepted reference, candidate, held first candidate, their interpreter modes, and CPython. Ratios below are repeated candidate/reference measurements; lower is better. Work is the probe's complete internal elapsed interval; CPU and peak RSS cover the process.

| Saved operation | Calls | JIT work | Interpreter work | JIT CPU | JIT RSS | JIT work / CPython |
|---|---:|---:|---:|---:|---:|---:|
| Native length, summed results | 1,000,000 | 0.599 | 0.586 | 0.648 | 1.002 | 2.726 |
| List length | 100,000 | 0.595 | 0.588 | 0.835 | 1.006 | 3.158 |
| List copy | 100,000 | 0.740 | 0.731 | 0.870 | 1.008 | 3.145 |
| List reverse | 100,000 | 0.612 | 0.590 | 0.842 | 1.003 | 3.169 |
| Dictionary keys | 100,000 | 0.696 | 0.701 | 0.878 | 1.006 | 2.783 |
| String upper | 100,000 | 0.674 | 0.691 | 0.840 | 1.005 | 2.313 |
| String strip | 100,000 | 0.715 | 0.706 | 0.891 | 1.005 | 3.122 |
| String split | 100,000 | 0.889 | 0.876 | 0.932 | 1.007 | 7.290 |

Initial native work improvements were 12-41%. Repeated rejected `clear` work is 0.958/0.954 in JIT/interpreter mode; saved iterator work is 0.962/0.947, although its JIT process CPU is 1.007. Saved weakref call work is 0.979/0.971. The first candidate retained iterator and weakref call costs. Direct paired comparisons with that held binary give 0.871/0.907 for iterator work and 0.942/0.946 for saved weakref calls after the refinement. Its sources and every initial/repeated result remain under `target/performance/saved-native-leaf-v1-investigation/`.

The unchanged 24-application suite has three initial cycles. Geometric means for work/process elapsed/CPU/RSS are 0.998/0.989/0.991/0.995 against the reference in JIT mode and 1.016/1.003/1.002/0.999 in interpreter mode. Work excludes the startup-only fixture. Against CPython, the means are 1.024/1.385/1.241/1.559 for JIT and 2.229/2.025/1.922/1.329 for the interpreter.

Eleven selected applications have eleven repeat cycles, covering every initial cost above 3% in any measured metric plus fixed controls. Their reference means are 1.000/0.993/0.997/1.005 for JIT and 1.019/1.009/1.011/0.997 for the interpreter. These aren't repeated full-suite means. No general application or memory improvement is claimed.

Costs remain. Repeated keyword-call work is 1.037/1.035; independent 21-cycle diagnostics give 1.033/1.017. Eleven cycles at one million calls give 1.018/1.027, with CPU 1.020/1.025. Repeated Python bound-method JIT work is 1.029. Interpreter `jitloop` and nested-loop work costs persist in independent 21-cycle checks at 1.032 and 1.049; their process CPU ratios are 1.016 and 1.007. N-body interpreter work diminishes from 1.055 initially to 1.032 on repetition and 1.012 in diagnostics. The `pidigits` JIT RSS ratios are 1.046 initially, 1.032 on repetition, and 1.002 in diagnostics. Every batch is retained.

Two independent 31-cycle startup batches show approximately unchanged JIT elapsed time. Repeated elapsed ratios for ordinary/no-site/isolated/import startup are 0.999/1.003/0.994/1.004; CPU ratios are 1.000/1.029/1.007/1.009. Interpreter elapsed ratios are 0.981/0.963/0.982/0.991. Ordinary JIT startup remains about 1.69 times CPython. The two large GC controls have seven cycles only: weakref work is 0.991/0.986 and weak-set cleanup 1.000/0.995. Weakref work remains about 22 times CPython. CPython superiority remains unachieved.

## Validation and reproducibility

All nine harness tests, 412 VM tests, embedding's explicit 1 MiB stack case, 34 C API unit tests, direct weakref C API integration, VM Clippy with established exclusions, no-default compilation, and the release CLI build pass. Frozen validation passes 84 regressions plus five additional GIL-disabled owner checks, 315 probe shapes across CPython/reference/candidate modes, eight observer runs, 36 selected upstream groups, and twelve native database checks. Test counters confirm 1,601 optimized calls and 198 direct rejection hits; separate tests check body identity and weak-cache ownership. Native extensions can reenable the GIL; separate regressions cover GIL-disabled execution.

A pre-existing cleanup difference remains: CPython immediately clears a weakref watching a temporary native method, while the accepted reference and both candidates fail that assertion in JIT, interpreter, and GIL-disabled execution. The independent failing oracle is archived. The unwatched temporary destructor case passes. This change doesn't repair the watched-temporary behavior. The accepted reference also still fails the Windows cold-compilation gate: `sumvm` is 1.213 times the CI reference on retry, while its warm diagnostic is 1.000. This candidate hasn't been measured on Windows.

The candidate is 51,898,216 bytes, 3,936 bytes larger than the reference. Reference SHA-256: `6304bd221247a0d09285ececc59fd94044774709e3fbb28622594ee84571697a`. Candidate SHA-256: `cc6a0bab9bb87bc2bfc77553bce33aa7d86f3772b67d4cef73c30bd18f980ec9`. Runtime patch SHA-256: `ccf16ba7505ca7f5f80f514f3c7453c09b790613212e8b7f914cacb71a5a9d7a`.

Sources, binaries, validation logs, host snapshots, hashes, and all initial/repeated/diagnostic results are archived under `target/performance/saved-native-leaf-v2-investigation/` and its named sibling result files. Source identity and every case/sample count were verified after controllers exited. Reproduce with `tools/bench_compare.py`, `tools/bench_saved_native_methods.py`, `tools/bench_bound_calls.py`, and `tools/bench_startup.py`. Workloads, baselines, gates, collection thresholds, and JIT budgets are unchanged.
