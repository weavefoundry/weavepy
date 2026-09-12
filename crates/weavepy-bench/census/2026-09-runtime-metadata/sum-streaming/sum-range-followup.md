# Exact range sums

Unimplemented lead. Streaming sum avoids materializing a range but still walks
its elements through arithmetic dispatch. An exact range and exact integer
start have no iteration or addition callbacks. Their arithmetic-series sum can
be computed at full precision from count, first value, and step, without walking
the population. Use range_len_bigint and the real big-aware bounds, not the
potentially overflowing i128 length helper. Handle an empty range by returning
the original start value to preserve identity and Bool starts; defer noninteger
starts to the existing iterator/arithmetic path.

Before implementing, record CPython oracles over bounded ascending, descending,
empty, i64-edge, i128, and larger ranges; include starts with large integers,
bools, floats, and objects whose __add__ records every operand. Range.__index__
callbacks run at construction and must remain observable. Do not run enormous
reference loops. A formula is only valid after validating the exact builtin
range and arithmetic types; generator, custom iterator, foreign, and subclass
protocols must not be replaced. No implementation or performance claim yet.
