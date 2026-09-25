# Native scalar field-update experiments

The first native-entry candidate completes a checked integer field update after
the existing native-call preflight. It avoids another frame, argument buffers,
and pin table for that call. The receiver, function, code, defaults, argument
lanes, namespaces, recursion, observers, and retirement accounting keep their
existing guards. The evaluator checks both reads, the store, class and key
identity, exact integer values, and overflow before one final store. A result
lane mismatch preserves the completed value and resumes after the call.

This candidate also includes the interpreter shortcut from the
[earlier field-update experiments](PERFORMANCE-FIELD-UPDATE-EXPERIMENTS.md).
It remains unadopted while a narrower native-only candidate is evaluated:
compiled workloads improve substantially, but some fallback costs repeat.
No JIT admission, density, retirement, pin, or benchmark thresholds change.

## Measurements

The baseline is accepted runtime `4377ce1`, with documentation and fixture
changes through `b4d2004`. Measurements use standard release settings, the
Intel macOS host, optimized CPython 3.14.5, separate frozen caches, and
interleaved paired runs after discarded warmup. No owned build, test, or
profiling work overlaps timing. Desktop background activity was recorded;
small differences remain provisional. Ratios are candidate/baseline.

| Workload | Standard JIT | Standard interpreter | Sustained JIT | Sustained interpreter |
| --- | ---: | ---: | ---: | ---: |
| Richards-style fixture | 0.601 | 0.436 | 0.593 | 0.423 |
| Attribute access | 0.669 | 0.839 | 0.665 | 0.838 |
| Mixed calls | 0.899 | 0.916 | 0.911 | 0.907 |
| Mutable class value | 1.035 | 1.046 | 1.014 | 1.035 |
| Slot-backed update | 1.028 | 1.015 | 1.040 | 0.993 |
| Custom setter | 1.015 | 1.027 | 1.014 | 1.022 |

These batches use seven cycles. Sustained update controls use one million
calls; the application work counts are 250,000 for Richards-style scheduling,
750,000 for mixed calls, and 500,000 for attribute access. Sustained default-JIT
work still takes 1.934, 2.553, and 1.601 times CPython, respectively.

The unchanged six-mode probe records JIT ratios of 0.520 for parameters,
0.551 for constants, 0.605 for defaults, and 0.683 for saved methods. The
separate class-default probe records 0.437. These are actual native completions
where the original loop compiles; the class-default loop remains interpreted.
Twenty-one existing scalar, literal, field-argument, and fallback controls also
ran for seven cycles.

A three-cycle, 24-fixture suite gives JIT geometric means of 0.961 workload
time, 0.976 process elapsed time, 0.973 CPU, and 1.008 peak RSS against the
baseline. Against CPython, the corresponding ratios are 1.036, 1.393, 1.262,
and 1.563. Work time excludes the startup fixture. Thirty-one startup cycles
record normal/no-site/isolated elapsed ratios of 1.019/1.025/1.017. This
candidate doesn't establish overall parity, a startup improvement, or a
memory improvement.

## Validation and provenance

All 391 VM tests, embedding on the existing 1 MiB worker stack, VM Clippy with
the established exclusions, no-default compilation, and formatting pass.
Compiler encoding/layout checks and fourteen benchmark-tool tests carry
forward from identical sources. The frozen release passes 297 runs across
99 fixtures, 1,048 semantic probes, twelve CPython fixtures, and seven extra
constant/shape checks. All 53 probe and three application compilation-decision
traces match the accepted binary, with runtime statistics disabled.

Actual native-completion counters exceed 3,000 per 4,000-call parameter,
constant, default, and saved-method probe. Unsupported slot, hook, property,
mutable-class-value, and large-integer controls never enter the evaluator.
A dedicated result-lane test confirms that the completed integer store isn't
replayed after a caller compiled for a float result. Saved-method tests cover
class replacement, code replacement, receiver retention, and final release.

The frozen CLI is 51,877,544 bytes, SHA-256
`cdfeff6fc7b36fd1f8f18d889e1517a0b068a76fb9ae087389a2b38964f78386`.
Its patch against `b4d2004` is
`cf8127a532fd78c4d920e736e1bd8549938489c8d2e1e4b5cc76532e41b0f66b`.
Exact sources, traces, logs, and every measurement remain under
`target/performance/native-scalar-field-update-entry-investigation/` and the
`target/performance/native-field-update-rb4d2004-*` result files. Only the
regression fixtures and this report are committed at this stage.
