# Weakref wrapper helpers

Weakref wrappers no longer allocate private alive and clear functions. The getter remains the strong owner of the slot; registry entries and the slot's wrapper back-pointer remain weak. Callback ownership stays in the GC-traced instance dictionary. Exact refs now have three dictionary entries, and fallback proxies and subclasses have five. This removes two `BuiltinFn` allocations and two captured closure allocations per wrapper without changing object, instance, or slot-storage layouts. Slot construction also consumes its owned target instead of cloning it again.

The bundled C API shim now clears the whole registry batch through a shared private `_weakref` helper, matching the existing native C API operation. Previously, clearing the first wrapper could queue the other wrappers' callbacks, and proxy-only watchers could escape clearing. Both cases now clear every watcher without publishing callbacks. Ordinary destruction still runs callbacks in reverse registration order.

## Measurements

The reference is accepted commit `952f8c8933c69c50355e9cc8142f502ee6b1b97e`. Measurements use Intel macOS and CPython 3.14.5 with the GIL enabled and its JIT unavailable. Process order alternates; warmup is discarded, caches are warmed and checked for mutation, and setup, assertions, and normal collection remain timed. Owned builds, tests, and profilers don't overlap timers. The desktop remained active: initial snapshots include media analysis at 98% CPU and Chrome Renderer at 45%; repeat and diagnostic snapshots show no similarly busy background process. Small differences require caution.

Twelve focused cases have seven initial cycles and eleven repeats, each including reference/candidate JIT and interpreter modes plus CPython. Ratios below are repeated candidate/reference results; lower is better. Work is the complete internal probe interval; CPU and peak RSS cover the process.

| Workload | Size | JIT work | Interpreter work | JIT CPU | JIT RSS | Interpreter RSS |
|---|---:|---:|---:|---:|---:|---:|
| Weakrefs | 100,000 | 0.851 | 0.839 | 0.854 | 0.757 | 0.746 |
| Callback-bearing weakrefs | 100,000 | 0.890 | 0.900 | 0.893 | 0.797 | 0.828 |
| Watched cycles | 100,000 | 0.854 | 0.872 | 0.860 | 0.797 | 0.776 |
| Weak-set cleanup | 100,000 | 0.896 | 0.898 | 0.900 | 0.831 | 0.830 |
| Weak-key lookup | 100,000 | 0.879 | 0.875 | 0.887 | 0.993 | 0.990 |
| Weak-value lookup | 100,000 | 0.992 | 1.002 | 0.986 | 0.994 | 0.994 |

Weak-key/value probes retain 32 owners and perform 100,000 lookups; they aren't large retained populations. Weak-key lookup constructs temporary refs. Initial weakref/callback/cycle/weak-set work ratios were 0.868/0.892/0.877/0.905 in JIT mode and 0.842/0.897/0.863/0.899 in interpreter mode. Two additional GC cases have seven initial cycles only: finalizer work is 0.868/0.873 and frozen work 0.868/0.857, with JIT RSS 0.792 and 0.795.

Some costs remain. Saved iterator calls at 100,000 iterations measured 1.067/1.064 on repetition; independent 21-cycle diagnostics give 1.019/1.025. Eleven cycles at one million calls give 1.027/1.022, with process CPU 1.034/1.027. Saved weakref calls measured 1.025/1.030 on repetition, 1.009/1.027 in 21-cycle diagnostics, and 1.020/1.033 at one million calls; the long-run CPU ratios are 1.022/1.023. Repeated callable-proxy interpreter work is 1.024, while subclass, weak-value, and saved-repr calls are close to the reference. All batches are retained; the smaller diagnostic costs don't erase the earlier measurements.

The unchanged 24-application suite has three initial cycles. Geometric means for work/process elapsed/CPU/RSS are 0.976/0.986/0.984/0.997 against the reference in JIT mode and 0.984/0.994/0.992/0.992 in interpreter mode. Work excludes the startup-only fixture. Nine selected applications have eleven repeat cycles; their means are 0.998/0.986/0.989/1.001 and 0.995/0.998/0.997/0.998. These aren't repeated full-suite means. Initial N-body and Richards work costs diminish to approximately 1% or less on repetition. List JIT RSS remains 1.036 after an initial 1.111; 21 diagnostic cycles give 1.029 at the original size, and eleven cycles at ten times the work give 1.005. No general application or memory improvement is claimed.

Two independent 31-cycle startup batches show approximately unchanged elapsed time. Repeated JIT ratios for ordinary/no-site/isolated/import startup are 0.994/1.002/0.994/0.994; CPU ratios are 1.000/0.962/0.986/0.994. Interpreter elapsed ratios are 1.004/1.003/0.996/0.999.

CPython superiority remains unachieved. Full-suite JIT work/process elapsed/CPU/RSS means against CPython are 1.000/1.341/1.198/1.553; interpreter means are 2.151/1.950/1.844/1.324. Ordinary JIT startup is about 1.64 times CPython. Repeated weakref work is 18.28 times CPython and RSS 4.53 times; callback work is 29.13 times, weak-key lookup 32.30 times, and DeltaBlue application work 6.10 times. An aggregate work mean near parity doesn't establish parity for individual workloads or other metrics.

## Validation and reproducibility

All nine harness tests, 412 VM tests, embedding's explicit 1 MiB stack case, 34 C API unit tests, expanded direct weakref C API integration, VM Clippy with established exclusions, no-default compilation, and the release CLI build pass. Frozen validation passes 92 regression runs, five extra GIL-disabled saved-owner runs, five extra GIL-disabled no-callback runs, 476 probe shapes, eight observer runs, 42 selected upstream groups, and twelve native database checks. Full upstream `test_weakref` runs 137 tests with seven skips in each of JIT, interpreter, and GIL-disabled modes. Native extensions can reenable the GIL; separate regressions cover GIL-disabled execution.

The new portable clearing fixture covers mixed and proxy-only watchers, callable targets, cross-thread construction, saved public calls, repeat clearing, and re-registration. CPython runs it through its exported native API because this installation lacks `_testcapi`. A callback-batch fixture checks newest-first order, complete clearing before callbacks, reentrant collection, and cancelled watchers. It doesn't assert callback-attribute cleanup after cyclic collection: WeavePy already differs from CPython there. WeavePy's existing native no-callback API also removes callback attributes that CPython retains. The watched temporary native-method cleanup difference remains unchanged in all three execution modes. These separate differences aren't repaired here.

The reference commit still fails the Windows cold-compilation gate: `sumvm` is 1.403 times the CI reference on retry. Seven-cycle diagnostics give cold/warm work ratios of 1.387/1.007 for `sumvm`, 1.282/0.990 for nested loops, and 1.251/0.984 for `jitloop`, despite lower process elapsed times. Traces show four startup compiles in the CI reference and none at this branch's accepted head, giving different compiler warm-up contexts. This isn't proof of the entire cause, and this candidate hasn't been measured on Windows. Budgets, gates, baselines, and workloads are unchanged.

The candidate is 51,893,872 bytes, 4,344 bytes smaller than the reference. Reference SHA-256: `cc6a0bab9bb87bc2bfc77553bce33aa7d86f3772b67d4cef73c30bd18f980ec9`. Candidate SHA-256: `4ed09f70f29acc98ead036c13eb0d4c06657570af52a34012a447ff80cb123bc`. Production and C API test patch SHA-256: `dc175ef5453da333dd50daaf28dd841f36ce5a4b9fdcb8730c74197e031b4751`.

Sources, binaries, manifests, validation logs, host snapshots, and every initial/repeated/diagnostic result remain under `target/performance/weakref-wrapper-helper-v1-investigation/`. Counts, binary/source identity, and cache immutability were verified after controllers exited. Reproduce with `tools/bench_compare.py`, `tools/bench_gc_populations.py`, `tools/bench_set_mutations.py`, `tools/bench_weakref_access.py`, `tools/bench_weakref_methods.py`, `tools/bench_bound_calls.py`, and `tools/bench_startup.py`.
