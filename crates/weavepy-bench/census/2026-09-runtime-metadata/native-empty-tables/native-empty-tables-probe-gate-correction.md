# Correct the native-call coverage gate

The first gate stopped before timing: no-inner-args passed, but one-inner-arg
had zero method-call helper counts. Its actual bench and advance both compiled,
and the baseline recorded399,862 native-to-native calls, of which199,950 used
the scalar-leaf shortcut. The dynamic native helper calls try_native_call with
a receiver without charging the separately guarded method-call helper count
(tier2.rs near5772). The original method_calls requirement was too narrow.

Preserve the failed inputs and outputs under focused-initial. Require more than
100,000 ordinary native-to-native calls (all native calls minus scalar-leaf
calls), with the required functions compiled. Scalar recursion requires1,000.
This targets the changed buffer/table setup regardless of call dispatch helper.
Also run coverage through the same registered module shape as warm measurement.
No runtime source or candidate binary changed.
