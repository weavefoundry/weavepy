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
        # A `Lib/`-shaped entry (the conformance runner puts
        # `vendor/cpython/Lib` on the path) carries the test package as
        # a direct child.
        _child = _os.path.join(_norm, "test")
        if (
            _os.path.isfile(_os.path.join(_child, "__init__.py"))
            and _child not in __path__
        ):
            __path__.append(_child)
        # Running a file from `test/` itself or from a (possibly
        # nested) subpackage — `Lib/test/test_dataclasses/`,
        # `Lib/test/test_unittest/testmock/` — puts that directory on
        # `sys.path`; walk up to the enclosing on-disk `test/`.
        _d = _norm
        for _ in range(4):
            if _os.path.basename(_d) == "test" and _os.path.isdir(_d):
                if _d not in __path__:
                    __path__.append(_d)
                break
            _up = _os.path.dirname(_d)
            if _up == _d:
                break
            _d = _up
    except (TypeError, ValueError):
        pass
# When a full CPython regression suite is among the grafted directories,
# make it the package's *identity*: tests locate on-disk fixtures via
# `os.path.dirname(test.__file__)` (testpatch/test_pkgutil resolve
# `test_import/data`), and the frozen `__file__` points at the
# materialized stdlib tree, which carries only the support layer.
for _d in list(__path__):
    try:
        if _os.path.isfile(_os.path.join(_d, "test_import", "__init__.py")):
            _init = _os.path.join(_d, "__init__.py")
            if _os.path.isfile(_init):
                __file__ = _init
            break
    except (TypeError, ValueError):
        pass
del _os, _sys
for _n in ("_p", "_norm", "_child", "_d", "_up", "_init"):
    try:
        del globals()[_n]
    except KeyError:
        pass
del _n
