# Resolve bounded nested function return lanes for methods

The corrected one/three-inner-argument probes compile bench and advance, but
advance uses the dynamic native bound-method path. The zero-inner-call probe
uses the guarded method helper. All exercise ordinary try_native_call setup.
The dynamic cases allocate/pin method values, use more lookup work, and cross
38 pin-pressure exits in the two-call coverage diagnostic.

Source inspection explains this distinction: method_ret_info classifies any
global Python function other than the method itself as Opaque. Thus the nested
unary/ternary call prevents method return inference, and probe_method_entry
cannot burn the method as a stable scalar-returning call. The real advance
body compiles later using the normal, richer classifier.

An unimplemented candidate could use the existing callee_ret_info for one
level of nested plain-function return inference. That function itself keeps
other functions opaque, providing a finite depth bound. Before implementation,
audit its return-cache lifetime, namespace keys, recursion handling, and TLS
borrowing. Preserve the normal function signature/default checks. Predictions
must remain protected by actual return-tag checks, including after nested
global rebinding, code/default replacement, receiver mutation, exceptions,
integer overflow, or a callback that changes the return type.

Do not use the current compiled artifact's lane unguarded. Add oracles that
warm the method, change its nested helper, and compare JIT/interpreter/CPython
results and side-effect ordering. Require the guarded method helper count to
rise in this specific experiment, while ordinary native-call counts stay
covered. Measure constructor and recursive controls as well as these probes.
This is only a diagnosed eligibility limitation, not an implemented speedup.
