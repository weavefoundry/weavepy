# Runtime performance status

The runtime in this PR includes the work through the bounded native-call
scratch revision, called phase 79 in the research records. Subsequent JIT and
datetime experiments are preserved in the
[September 13 checkpoint](https://github.com/weavefoundry/weavepy/tree/c410f1ae7e157f9d53798af4180a77c8f385f2a5/crates/weavepy-bench/census/2026-09-runtime-metadata/).
They have not been overlaid onto the active implementation. WeavePy has not
achieved the objective of outperforming CPython across every meaningful metric.

## Implementation

- Shared string, bytes, and tuple owners reduce Object and DictKey from 24 to
  16 bytes on ARM64. Ownership, weak references, tuple hash caching, native
  pins, tracing, and C API buffer lifetimes retain regression coverage.
- Lazy metadata, compact slot storage, shared slot names, and collector scratch
  reuse reduce allocation and retained-storage overhead. Tracing and finalizer
  changes have lifecycle and cross-thread regression tests.
- Native numeric reductions, range collection, float formatting, JSON buffer
  handling, supported pickle encoding and decoding, and datetime fast paths
  avoid repeated Python-level work. Unsupported inputs retain fallback paths.
- JIT changes cover constants, scalar calls and results, list iteration and
  construction, guarded native entry, and exception exits. Callback,
  deoptimization, frame-observation, and temporary-pin lifetimes have tests.
- Native-call scratch pools reuse storage with a 64-entry cap and a 16 KiB
  retained element-capacity limit per element type. The two pools retain at most
  32 KiB of element capacity per thread. This excludes active calls, allocator
  overhead, and pool headers; it is not a bound on process RSS.

The retained changes have workload-specific tradeoffs. This checkpoint is not
an assertion that every optimization improves every workload.

## Measured results and limitations

These measurements compare each change with its own recorded predecessor on
macOS ARM64. They must not be multiplied together or interpreted as one
end-to-end speedup.

| Change | Measured result | Limitation |
| --- | --- | --- |
| Shared value storage | 2.34% lower geometric-mean peak RSS across 24 processes; all 24 improved | Some short allocations and calls regressed |
| Shared-value populations | About 6% lower peak RSS; shared dictionaries about 9.5% lower | Extra metadata can increase small-object allocation sizes |
| Public `time.isoformat` | About 37% to 39% less execution time | Some fallback cases regressed; full-suite time was approximately flat |
| Native scratch consolidation | About 1.5% less full-suite time against the retained storage reference | The subsequent capacity limit has no qualified timing samples |

The [storage report](https://github.com/weavefoundry/weavepy/blob/c410f1ae7e157f9d53798af4180a77c8f385f2a5/crates/weavepy-bench/census/2026-09-runtime-metadata/thin-value-storage/REPORT.md),
[time-formatting report](https://github.com/weavefoundry/weavepy/blob/c410f1ae7e157f9d53798af4180a77c8f385f2a5/crates/weavepy-bench/census/2026-09-runtime-metadata/ascii-time-format/REPORT.md),
and [scratch report](https://github.com/weavefoundry/weavepy/blob/c410f1ae7e157f9d53798af4180a77c8f385f2a5/crates/weavepy-bench/census/2026-09-runtime-metadata/native-scratch-byte-budget/REPORT.md)
retain their methods, regressions, and provenance. The earlier
[JSON](PERFORMANCE-JSON.md) and
[collections and datetime](PERFORMANCE-COLLECTIONS-DATETIME.md) reports describe
their historical revisions, not a fresh measurement of the final runtime.

The latest fully measured isolated experiment, phase 95, took about **3.43 times
CPython's workload time** over 23 workloads and used about **2.02 times CPython's
peak RSS** over 24 processes. It won six workload-time comparisons and no RSS
comparisons. Its older 21-workload cohort was about 2.11 times CPython's time.
Cohort changes affect the headline, so compare identical sets of workloads.
These are geometric means, and this experiment is not the active phase 79
runtime. Phase 79's final capacity limit remains unmeasured because its load
gates stopped before launching any benchmark.

The separate scalar-list read experiment improved warm indexed reads about
21.6 times against its predecessor and reached about 2.17 times CPython's speed
on those focused cases. Small RSS increases remained. Such focused wins do not
establish whole-suite superiority. Phase 96's initializer guards passed
correctness validation but have no performance measurements.

## Validation and reproduction

At checkpoint `c410f1ae`, the active runtime's 340 source hashes matched its
validated build: 353 VM tests, 156 C API tests, 99 targeted checks, 44 independent
CPython result checks, and 275 compatibility checks passed, together with
formatting, strict lint, and no-JIT checks. Those results describe that
checkpoint. Subsequent Windows portability fixes gate a Unix-only import and
share the lazy instance dictionary in the DLL-directory helper; the PR's CI
runs validate the updated sources on Linux, macOS, and Windows.

Build the CLI and retain a separate executable for the revision being compared:

```sh
cargo build --release -p weavepy-cli --bin weavepy
mkdir -p target/performance
python3.14 tools/bench_compare.py \
  --base /path/to/reference/weavepy --new target/release/weavepy \
  --samples 5 --out target/performance/suite.json
python3.14 crates/weavepy-bench/census/2026-09-runtime-metadata/probes.py \
  --base /path/to/reference/weavepy --new target/release/weavepy \
  --samples 7 --out target/performance/probes.json
```

The [runtime tool directory](../crates/weavepy-bench/census/2026-09-runtime-metadata/)
retains reusable probes, differential oracles, the compatibility driver, and
its required tuple C API test selection. Use each probe's `--help` for its
arguments. Build and validate both binaries before timing; use consistent
launch conditions, separate caches for different binaries, and unchanged
workload definitions. Keep cold and warm measurements separate and retain all
paired samples, including losses. Timed workload, process elapsed time, process
CPU time, and peak RSS are different metrics.

Generated output belongs under `target/` or in separate research storage. Raw
logs, copied sources, manifests, and compressed experiment archives are excluded
from the final PR tree. They remain locally and in the checkpoint history.
The [checkpoint inventory and limitations](https://github.com/weavefoundry/weavepy/blob/c410f1ae7e157f9d53798af4180a77c8f385f2a5/crates/weavepy-bench/census/2026-09-runtime-metadata/CHECKPOINT-2026-09-13.md)
describe what was saved. Known fold-index coercion and native-frame identity
differences remain unresolved. These results make no universal energy, scaling,
32-bit execution, or controlled build-time claim.
