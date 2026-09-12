# Generic tuple length hints

Untargeted behavior recorded before the exact-range optimization. The probe
in range-collection-protocol-probe.py returns an independent tuple iterator
from Source.__iter__. CPython calls Source.__len__ when building a list, but
doesn't call it when building a tuple. The preceding WeavePy release calls
it in both cases. Both runtimes call __iter__ exactly once and produce the
same contents. The original regression assumption that both constructors
must invoke the source length hook once was wrong and failed on CPython.

Exact range collection doesn't alter that generic path. The corrected range
fixture asserts iteration and contents for both constructors and the list
length hook, with an explicit comment documenting the tuple difference.
Raw CPython and preceding-release protocol outputs are preserved separately.

A future repair should audit list and tuple iterator creation, the object
whose __len__/__length_hint__ is consulted, callback order, exception handling,
and iterable/iterator lifetime. collect_iterable also serves other consumers;
changing it globally without separating each consumer's protocol would risk
introducing callback or error-order regressions. No such repair is applied.
