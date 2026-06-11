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

# CPython resolves ``test.<name>`` submodules against this package's
# ``__path__``. WeavePy ships ``test`` (and ``test.support``) frozen, so
# the package has no backing directory by default — which means a
# vendored test that imports a *sibling* test module (e.g.
# ``from test import test_contextlib`` in ``test_contextlib_async``, or
# ``test.pickletester``) can't find it. Point ``__path__`` at any on-disk
# ``test/`` directory currently on ``sys.path`` (a checked-out
# ``Lib/test/`` is ``sys.path[0]`` when its files are run directly), so
# those siblings load from disk. Frozen modules still win — the import
# machinery consults the frozen registry before walking ``__path__`` — so
# ``test.support`` keeps using the faithful frozen port.
import os as _os
import sys as _sys

try:
    __path__
except NameError:
    __path__ = []
for _p in _sys.path:
    try:
        if not _p or not _os.path.isdir(_p):
            continue
        _norm = _os.path.normpath(_p)
        if _os.path.basename(_norm) == "test" and _norm not in __path__:
            __path__.append(_norm)
            continue
        # Running a file inside a *subpackage* of `test/` (e.g.
        # `Lib/test/test_dataclasses/__init__.py`) puts that subpackage
        # directory on `sys.path` — its parent is the on-disk `test/`.
        _parent = _os.path.dirname(_norm)
        if (
            _os.path.basename(_parent) == "test"
            and _os.path.isdir(_parent)
            and _parent not in __path__
        ):
            __path__.append(_parent)
    except (TypeError, ValueError):
        pass
del _os, _sys
try:
    del _p
except NameError:
    pass
