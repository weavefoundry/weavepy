# Claim finalizers once at dispatch

Concurrent reclamation could invoke `__del__` twice on the same allocation. A diagnostic on b216ac9 recorded duplicate destructor entries with the same object identity and unique constructor token, each with exactly one completed constructor. The exact pair of competing scheduling paths wasn't established.

Dispatch now atomically claims the allocation's existing `finalize_ran` flag before executing Python, closing a generator, or issuing an unawaited-coroutine warning. Duplicate requests neither invoke the finalizer nor publish another request's completion. The last-Arc instance-drop fallback queues an unclaimed copy; failed enqueue suppresses that copy's recursive drop fallback. Object layouts, collection thresholds, JIT admission, and benchmark inputs are unchanged.

## Correctness

A deterministic regression fails before the fix, reporting two dispatches instead of one, and passes afterward. It covers instances, generators, last-Arc drop, and reentrant queue-borrow failure. A Python fixture verifies unique finalization tokens across two threads and cyclic resurrection. Five alternating runs of the original CPython 3.14.5 `test_gc.GCTests.test_trashcan_threads` fail on b216ac9 and pass on the candidate under `-X gil=0`. Three additional token diagnostics each complete all 24,400 constructors and finalize each unique token exactly once.

All 401 VM tests pass, along with embedding's 1 MiB stack check, VM Clippy with established exclusions, no-default compilation, formatting, and fourteen benchmark-tool tests. The frozen release passes 318 runs across 106 regression fixtures, 1,048 semantic probes, nineteen CPython fixtures, 180 ordinary-population checks, and 100 GC-population checks. All 53 probe, three application, and nine population JIT compilation decisions are unchanged.

All nine unmodified upstream GC, weakref, and weak-set suites report success in JIT, interpreter, and GIL-disabled modes. Their counts are 57 tests with twelve skips, 137 with seven skips, and 46 respectively. The GIL-disabled weakref suite nevertheless emits an ignored worker-thread error: `SET_FUNCTION_ATTRIBUTE on a shared function`. A bounded diagnostic running CPython-produced bytecode while another thread holds `gc.get_objects()` snapshots reproduces that exact error on both b216ac9 and the candidate; CPython passes. Six alternating focused upstream deepcopy repetitions don't reproduce it. This is a separate, unresolved function-construction/GC-sharing gap, not erased by the suite's success status. All correctness and diagnostic timings are excluded.

## Measurements and retained costs

Paired measurements use Intel macOS and optimized CPython 3.14.5. All owned builds, tests, profiles, and other timing controllers ended before timing. Background browser, rendering, metadata, and media-analysis activity was recorded. Ratios below are candidate/b216ac9; lower is better.

The unchanged 24-fixture suite, three cycles, gives these geometric means. Work time excludes startup; process metrics include it.

| Metric | JIT/accepted | Interpreter/accepted | JIT/CPython |
| --- | ---: | ---: | ---: |
| Work time | 1.008 | 1.012 | 0.988 |
| Process elapsed | 1.012 | 0.998 | 1.339 |
| Process CPU | 1.015 | 0.995 | 1.206 |
| Peak RSS | 1.001 | 0.992 | 1.545 |

Eleven-cycle repeats retain Fannkuch work ratios of 1.042/1.043 for JIT/interpreter, JSON JIT work/CPU of 1.037/1.064, attribute-access JIT work/CPU of 1.025/1.038, and deque work of 1.020/1.020. JSON JIT RSS is 1.019. Initial larger n-body interpreter (1.116), AES JIT (1.097), generator JIT (1.129), and pickle interpreter (1.116) increases don't repeat: their work ratios are 0.957, 0.999, 0.962, and 0.975. Generator interpreter work remains 1.018. The original samples are retained; incidental gains aren't attributed to the finalizer claim.

Seven-cycle measurements at 10,000 and 100,000 objects cover self-cycles, finalizers, weakrefs, callbacks, and frozen cycles, including their lifetime assertions. At 100,000 objects, finalizer work is 1.006/1.007 and RSS 0.994/1.002. Eleven-cycle repeats don't reproduce the initial small-cycle, large-cycle JIT, small-weakref interpreter, or small-frozen-cycle JIT costs. Repeated 100,000-callback work remains 1.014/1.039, with CPU 1.014/1.040 and RSS 0.991/0.981. Large-cycle interpreter RSS remains 1.010. These costs remain open.

Thirty-one-cycle normal/no-site/isolated/import startup gives JIT elapsed ratios of 1.023/1.022/1.028/1.012, CPU ratios of 1.021/1.064/1.048/1.014, and RSS ratios of 1.006/1.002/1.005/1.005. Normal startup still takes 1.650 times CPython elapsed.

This revision is retained to fix proven duplicate finalization before further collector changes, with the measured costs recorded. It doesn't establish overall CPython parity: process time, CPU, memory, individual workloads, and separate compatibility gaps still lag.

## Provenance

The frozen CLI is 51,881,568 bytes, 112 bytes above b216ac9, SHA-256
`000a52ac3edd8d3768e922343fbd4cda53d18a9e7900dbf3d8d50ad188971f4f`.
Its runtime patch against `b216ac9` is
`617981ede5d748e9fd1ac3df40d16ce776008c5d4209ba7b71c04508f60a4ab8`.
Source hashes and snapshots identify the inputs to the frozen binary. Upstream tests come from CPython commit `5607950ef232dad16d75c0cf53101d9649d89115`.

Raw samples, source snapshots, diagnostics, validation, and executable identities stay under `target/performance/threaded-finalization-investigation/`. Broad, startup, and application repeats use `target/performance/finalizer-dispatch-rb216ac9-*`. Reproduction uses `tools/bench_compare.py`, `tools/bench_startup.py`, and the unchanged `tools/bench_gc_populations.py` with the frozen b216ac9 and finalizer binaries.
