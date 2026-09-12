# Constant loads and scalar-result census

This archive records the validated scalar-result release before unboxed generic-call
results. Its executable is preserved as `target/release/weavepy-runtime-scalar-guards`.
All 196 compatibility checks pass, along with 45 JIT tests, 283 VM tests,
Clippy, workspace checks, and compilation without the JIT. Focused traces
cover canonical constants, guarded tuple lengths, integer call results,
enumeration, list builders, and temporary-list lifetimes. The checked call
matrix includes activations that cross the runtime pin limit.

The standard suite pairs the preceding small-slot release, the original
checkpoint, and CPython with the candidate. Focused callback probes preserve
both gains and remaining generic-call overhead. A separate comparison with
the initial constant-load release checks the shared-helper inlining change.
The earlier string-load regression and failed integer-division candidate
remain in their original archives. CPython still wins many time and memory
comparisons.

All archived inputs match the checksums in environment.json. The source diff
and input-sources directory come from the frozen candidate snapshot, not the
later working tree. Header sizes come from the retained optimized release
LLVM constants and exclude backing allocations and allocator overhead.
