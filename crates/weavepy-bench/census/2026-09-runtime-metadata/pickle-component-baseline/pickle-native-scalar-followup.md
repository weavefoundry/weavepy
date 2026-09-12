# Explore a native pickle path for exact built-in graphs

Unimplemented lead. The full census still measures pickle at roughly 330 times
CPython workload time. The repository's _pickle.py wraps the pure Python
implementation primarily to match accelerator error behavior. The fixture
contains both a graph of exact built-in containers/scalars and user class
instances. A native serializer for safe exact built-in graphs might reduce a
substantial part of the cost, but no implementation or savings are established.

First profile representative payload and instance graphs separately, retaining
all returns, serialized bytes, identity relationships, recursive graphs, and
protocol behavior against the recorded CPython. Inspect the repository's
CPython source and pure implementation for memoization and framing details.
A fallback must occur before callbacks or observable partial serialization.
Do not bypass subclasses, __reduce__, persistent_id, dispatch_table,
reducer_override, buffer callbacks, file writes, or reentrant pickler state.
A graph preflight must terminate on cycles and avoid recursion overflow. Include
empty/tiny inputs, shared references, non-ASCII and lone-surrogate strings,
large integers, float bit patterns, sets and tuple cycles, failure behavior,
protocol flags, and GIL-disabled access. Peak RSS and serialized size matter,
not only the loop timer. Don't introduce a fixture-specific recognizer.

This is a future architectural lead, not part of the decoder allocation trial.
