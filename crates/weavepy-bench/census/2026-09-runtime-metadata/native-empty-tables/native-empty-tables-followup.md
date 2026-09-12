# Skip empty native callee tables

The preceding scratch-buffer trial is archived and removed from runtime source.
This candidate changes only native-to-native setup: when a callee compilation
has no function or method tokens, its corresponding resolved table is None
without a TLS cache lookup or an Rc clone. The artifacts are immutable for the
compilation lifetime. A generation change cannot add tokens to these artifacts.
Nonempty tables still resolve through the generation-checked cache. Self-calls
still reuse the caller tables. Guards, pin cleanup, recursion ticks, stack
storage, deopt reconstruction, and table refresh are unchanged.

Existing nested-call, code/default-rebinding, constructor, exception, and
recursion tests are relevant. The scratch regression fixture remains useful
for general call-buffer correctness after removing its implementation trial.
New performance probes must prove the outer loop and called method compile and
that native-to-native method calls occur; the preceding original probes didn't.
