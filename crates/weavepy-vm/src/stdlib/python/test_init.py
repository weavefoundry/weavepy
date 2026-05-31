"""The ``test`` package — home of WeavePy's CPython-shaped regression
harness.

CPython's own ``Lib/test/`` is *not* vendored here (see RFC 0034 /
RFC 0020); this package supplies the ``test.support`` helper layer and
the ``test.libregrtest`` runner so a checked-out CPython ``Lib/test/``
(pointed at via ``$WEAVEPY_CPYTHON_LIB``) — or the bundled self-host
fixtures — can be discovered and run by ``weavepy -m test``.
"""

# Mirror CPython: importing the package shouldn't drag in the whole
# harness, so ``support`` / ``libregrtest`` are imported lazily by the
# things that need them.
__all__ = []
