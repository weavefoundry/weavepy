# Numeric sum fast paths and fallback regressions

Unimplemented lead from the completed focused sum measurements (the full census
is still running). Generic float/mixed/big-last reductions regress about 13 to
20 percent after replacing snapshots with per-element iterators. Extend pure
numeric handling only with CPython-compatible arithmetic and callback rules.

Primary source inspected through the web tool:
https://raw.githubusercontent.com/python/cpython/v3.14.7/Python/bltinmodule.c
Relevant regions: CompensatedSum at2578–2616, builtin_sum_impl at2632 onward,
integer block2674–2724, float block2725–2769, complex block2770 onward.
The compensation follows Neumaier; preserve operation ordering, negative zero,
and the rule that nonfinite compensation isn't added to the high part.

Critical control-flow detail: the integer, float, and complex fast blocks are
visited in order, once. A C-sized integer overflow or a large-integer start can
put subsequent operations into the generic loop permanently. Don't keep a wide
integer prefix and then automatically enable compensated float addition; that
can differ from CPython after an earlier i64 overflow even if the prefix later
cancels to a small integer. A Bool start also misses PyLong_CheckExact and begins
in generic arithmetic. A callback returning a float after entering the generic
loop does not automatically reenable the earlier float fast block.

Pure exact list/tuple inputs could avoid per-element iterator locks and VM
arithmetic dispatch. All Int/Bool/Long sequences can use a checked small total
and promote to BigInt. Preserve the original start object for empty inputs.
Mixed Float sequences need explicit states matching the source above, including
initial ordinary addition when leaving the integer block and a generic-numeric
state after integer overflow. Don't call Python while borrowing list contents;
nonprimitive inputs must keep the existing streaming callback path. An early
validation pass is safe only if it has no Python-visible callbacks.

Before applying, record exact CPython oracles for cancellation, i64 overflow
followed by floats, large starts, bool starts, Float/Int subclasses, callbacks
returning floats, NaN/infinity/signed-zero, and transitions to complex values.
Include raw floating bits where useful, and test fallback behavior separately.
The current47caseoracle has two known compensated-float mismatches (0.0 vs1.0)
and three TypeErrorwording differences. Don't hide new failures by broadening
that allowlist. No numeric follow-up implementation or new oracle has run yet.
