# Fixed weakref storage

Exact weakrefs on 64-bit targets now store their getter and callback in the existing fixed slot representation. A process-wide layout shares the two keys, and the instance dictionary stays unallocated. Proxies, subclasses, foreign class overrides, and 32-bit targets retain dictionaries. Object and instance layouts, generic slot lookup, and generic GC traversal are unchanged; `SlotStorage` remains 32 bytes and `PyInstance` 128 bytes.

Native field access requires layout allocation identity, not equal slot names. The inline-values flag only filters out ordinary instances. Native state remains authoritative if a dictionary is materialized or populated with implementation-looking keys. Callback clearing uses a storage flag selected before registration. The getter still owns the slot; registry entries and wrapper back-pointers remain weak. Callbacks remain visible to the existing GC traversal, and callback ownership is released after the slot borrow ends.

## Measurements

The reference is commit `02339c7bac75e5fd59268bc1f83086560f0f365d`. Measurements use Intel macOS and CPython 3.14.5, with its GIL enabled and JIT unavailable. Each batch alternates five engines: reference/candidate JIT and interpreter modes, plus CPython. Warmup is discarded, caches are checked for mutation, and setup, assertions, and normal collection remain timed. Owned builds, tests, and profilers don't overlap timers. The desktop remained active: snapshots include Chrome Renderer at 72% CPU initially, media analysis at 100% before repeats, and metadata indexing at 107% before diagnostics. Small differences require caution.

Twelve focused cases have seven initial cycles and eleven repeats. Ratios below are repeated candidate/reference results; lower is better. Work is the complete internal interval; CPU and peak RSS cover the process.

| Workload | Size | JIT work | Interpreter work | JIT CPU | JIT RSS | Interpreter RSS |
|---|---:|---:|---:|---:|---:|---:|
| Weakrefs | 100,000 | 0.886 | 0.916 | 0.892 | 0.786 | 0.799 |
| Callback-bearing weakrefs | 100,000 | 0.874 | 0.875 | 0.875 | 0.840 | 0.783 |
| Watched cycles | 100,000 | 0.872 | 0.894 | 0.877 | 0.789 | 0.805 |
| Weak-set cleanup | 100,000 | 0.868 | 0.854 | 0.871 | 0.809 | 0.785 |
| Weak-key lookup | 100,000 | 0.949 | 0.935 | 0.956 | 0.998 | 0.993 |
| Saved weakref calls | 100,000 | 0.957 | 0.918 | 0.969 | 0.997 | 0.991 |

Weak-key/value probes retain 32 owners, not 100,000; weak-key lookup creates temporary refs. Initial weakref/callback/cycle/weak-set work ratios were 0.893/0.851/0.875/0.862 in JIT mode and 0.895/0.852/0.886/0.832 in interpreter mode. Two additional GC cases have seven initial cycles only: finalizer work is 0.903/0.915 and frozen work 0.898/0.866, with JIT RSS 0.807 and 0.817.

Repeated saved-iterator work at 100,000 calls is 1.056/0.967, subclass access 1.062/1.016, weak-value lookup 1.068/1.086, and saved repr 1.006/1.042. Independent diagnostics use 21 cycles at the original size and eleven at one million calls. At the original size, iterator/subclass/weak-value/saved-repr work ratios are 1.057/1.024/1.008/0.993 in JIT mode and 1.036/1.018/0.991/1.003 in interpreter mode. At one million calls they are 0.981/1.031/1.008/0.977 and 0.985/1.034/1.008/0.990. The longer subclass cost persists. Every batch is retained; later measurements don't erase earlier costs.

The unchanged 24-application suite has three initial cycles. Work/process elapsed/CPU/RSS geometric means are 1.002/0.999/1.004/0.995 against the reference in JIT mode and 1.002/0.998/0.996/0.990 in interpreter mode. Work excludes the startup-only fixture. Twelve selected applications have eleven repeat cycles; their means are 1.009/1.007/1.015/0.995 and 1.015/1.007/1.007/0.993. These aren't repeated full-suite means. Five remaining application costs receive independent diagnostics at the original size and ten times the work. Longer N-body, AES, DeltaBlue, and sum-loop work ratios are within about 2.2% of the reference; DeltaBlue interpreter RSS is 1.005 after original-size diagnostic RSS of 1.032. Dictionary work remains slower: 1.048/1.030 in 21 original-size cycles and 1.063/1.037 in eleven cycles at one million iterations, with long-run CPU ratios 1.063/1.039. No general application improvement is claimed.

Two independent 31-cycle startup batches retain a no-site cost: initial/repeated JIT elapsed ratios are 1.020/1.032, and CPU ratios 1.024/1.040. Repeated ordinary/isolated/import startup elapsed ratios are 1.000/1.017/1.014. Interpreter ratios for ordinary/no-site/isolated/import startup are 0.980/0.974/0.988/1.001. There is no startup improvement claim.

CPython superiority remains unachieved. Full-suite JIT work/process elapsed/CPU/RSS means against CPython are 1.023/1.384/1.239/1.552; interpreter means are 2.178/1.997/1.889/1.316. Ordinary JIT startup is 1.67 times CPython. Repeated weakref work is 16.04 times CPython and RSS 3.76 times; callback work is 25.62 times, weak-key lookup 28.99 times, and weak-set cleanup 28.58 times. The targeted memory and construction gains don't establish parity elsewhere.

## Validation and limitations

All nine harness tests, 414 VM tests, embedding's explicit 1 MiB stack case, 34 C API unit tests, expanded direct weakref C API integration, VM Clippy with established exclusions, no-default compilation, and the release CLI build pass. Frozen validation passes 96 regression runs, five extra GIL-disabled saved-owner runs, five extra GIL-disabled no-callback runs, 476 probe shapes, eight observer runs, and twelve native database checks. Five additional runs of the maintained replacement-dictionary fixture pass on CPython, the reference interpreter, and all three candidate modes. New tests cover layout identity, independent values, dictionary materialization, colliding subclass slot names, callback clearing, and copy/deepcopy identity.

Upstream validation is qualified. All 60 selected group commands exit successfully, including 137 `test_weakref` tests with seven skips per mode, but the strict log audit fails on one ignored GIL-disabled thread exception: `SET_FUNCTION_ATTRIBUTE on a shared function` during weak-key dictionary deepcopy. JIT and interpreter logs are clean. A separate identical-bytecode oracle reproduces the same exception in three of three GIL-disabled runs on both the accepted reference and candidate while another thread holds `gc.get_objects()` snapshots; CPython passes. The function construction implementation is unchanged. Original failed logs and the reproducer are retained. This is a confirmed existing race, not a clean upstream pass.

Generic weakref reflection already differs from CPython and changes representation here. CPython rejects `vars(ref)` and returns `None` from `ref.__getstate__()`; the reference exposes its implementation dictionary. The candidate exposes an empty dictionary through `vars` and its two native fields through the default slot-based state. Copy/deepcopy preserve identity in all engines; the existing pickle exception mismatch remains. Callback-attribute differences after cyclic or silent clearing and the watched temporary native-method lifetime difference also remain unchanged.

The reference still fails Windows' cold-compilation gate: `sumvm` measures 1.169 against the CI reference initially and 1.231 on retry. Seven-cycle diagnostic cold/warm work ratios are 1.218/1.000 for `sumvm`, 1.158/1.002 for nested loops, and 1.149/1.001 for `jitloop`, despite lower process elapsed times. Traces show four startup compiles in the CI reference and none at this branch's accepted head, giving different compiler warm-up contexts. That isn't proof of the entire cause. This candidate hasn't been measured on Windows; budgets, gates, baselines, and workloads are unchanged.

The candidate is 51,895,064 bytes, 1,192 bytes larger than the reference. Reference SHA-256: `4ed09f70f29acc98ead036c13eb0d4c06657570af52a34012a447ff80cb123bc`. Candidate SHA-256: `27bc5737f43030d31b2ec98b286f5ab6993a67a99fbae7c9d0519c3f76b05f13`. Runtime patch SHA-256: `2c3211acc60ea34927ca9fe03ed566b9da29d98e0f6a72327dd14577d3625cae`.

Sources, binaries, manifests, validation logs, host snapshots, and every timing batch remain under `target/performance/weakref-fixed-layout-v1-investigation/`. Reproduce using `tools/bench_compare.py`, `tools/bench_gc_populations.py`, `tools/bench_set_mutations.py`, `tools/bench_weakref_access.py`, `tools/bench_weakref_methods.py`, `tools/bench_bound_calls.py`, and `tools/bench_startup.py`.

All eighteen diagnostic jobs, earlier timing batches, frozen inputs, and validation summaries were verified after their controllers exited. Eight-second dictionary profiles on each frozen binary show similar hot paths and do not explain the cost. The storage change is retained for its repeated construction and memory gains, with the dictionary, subclass, and no-site startup costs left explicit. A later inline-hint build emitted identical machine-code sections and adds no performance evidence; its ineffective hint is omitted. The profile is not timing evidence.
