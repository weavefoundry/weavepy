# Native binary-slice coverage lead

Unimplemented; no diagnostic run or speed evidence yet. Analyze has ListSlice
and StrSlice IR for BuildSlice + BinarySubscr and folded constant-slice forms,
but no OpCode::BinarySlice match. The compiler emits BinarySlice for dynamic
unit-step two-part slices (compiler/lib.rs~9390) and augmented slice reads~3413.
Existing JIT traces reject dirname for BINARY_SLICE. This is a coverage gap,
not proof that it dominates a standard fixture. list_ops uses acc[8:24], an
all-constant folded slice, so that particular slice already follows the
supported form. Do not attribute its large slowdown to BinarySlice without
actual coverage diagnostics.

A bounded change could reuse the exact-list/string unit-step helper, while
adding inference and emission for the three-operand BinarySlice. Preserve None
bounds, integer guards, subclass/__index__ fallback, overflow/clamping, helper
pin capacity, callback once, and error PCs. Audit the deopt snapshots: current
ListSlice/StrSlice point at the preceding erased BuildSlice or LoadConst and
reconstruct that producer's operands. A direct BinarySlice must resume at its
own pc with the original container/start/stop stack, including implicit None
bounds. Don't blindly share a producer-specific deopt layout.

Prepared target/inspect_standard_jit_coverage.py records full native traces for
list_ops/attr_access/call_overhead/json_bench/generators, modest work300, threshold3,
exact source/binary hashes. Diagnostic timings are not benchmark results.
Run only after BOTH focused and full census sessions finish, with no active
builds/tests/benchmarks/profilers. Its runtime-stage guards help avoid overlap.
No native coverage diagnostic has run in the current stage yet.

Additional correctness audit, not yet dynamically reproduced: both native
slice helpers encode missing bounds as i64::MIN, and their lowerers pass an
actual present Int bound unchanged. const_slice_shape also accepts everyi64.
A real stop=-9223372036854775808 may therefore collide with absent stop and
return a full slice instead of empty. Check helper clamping and guard coverage.
Draft target/slice_bound_diagnostic.py exercises dynamic explicit-None-step
BuildSlice and folded constant slices for lists and strings over100 calls.
Run with CPython and b63JIT/interpreter, retaining native-entry/compile proof;
this diagnostic is NOTRUN while GC40290 validates/measures.

Another unverified edge: analyzer accepts BuildSlice2/3 but slice deopt lowerers
always spill step=None as a third bound. Audit actual generated forms and
failure-pressure deopts before adding BinarySlice support; distinguish two-
bound, three-bound, and folded-constant origins in reconstruction if needed.
Any discovered defect should be repaired and regression-tested before widening
native slice coverage. Do not attribute either potential bug to GC bytefields.

Slice sentinel bug dynamically CONFIRMED before93286 by
target/slice-bound-diagnostic.json (CP,67e7JIT,b63JIT,b63interp). Dynamic
explicit-None-step list_bound/string_bound compile and returnlength3 instead0
forMIN stop; onlyJITdoesit,bothreleases. Constant-negativeMINsyntax remains
correct through unsupportedBINARY_SLICE fallback, not constantfoldedmarker.
Draft target/native_slice_bounds_regression.py coversMIN/nearMIN/MAX, absent
bounds, ASCII/Unicode/surrogates, listlanechanges, bigints, and__index__once.
Draftonly, notexecuted during93286.
Aminimalcandidate cannormalizeaPRESENT helperstopMIN to0 (bothclamp0 forany
validRustslicelength), retainoriginalvalueforfallbackspills, andleaveABSENT
stopMINunchanged. Twoinstructioncompare/select, constantfoldsforconstantbounds.
NoABIchange; no needtouseMIN+1. Notimplemented ormeasured.

Prepared NOTRUN target/slice_two_bounds_diagnostic.py uses validco_code
replacement: in sliced(value,stop) returningvalue[:stop:None], replaces the
explicitNone-stepLoadConst withNOP andBuildSlice3 withBuildSlice2, preserving
byteoffsets. WarmASCII thenUnicodeforcesnativehelperfallback. CompareCP and
b63JIT/interp after93286. Thiscanconfirm/refutetheextraNone spill hypothesis.
