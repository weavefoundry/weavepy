"""``test.libregrtest.findtests`` — locate the test directory and the
``test_*.py`` modules under it.

A faithful subset of CPython 3.13's
``Lib/test/libregrtest/findtests.py``. The test directory is resolved
from (in order) ``$WEAVEPY_CPYTHON_LIB``, a ``vendor/cpython`` checkout,
the bundled ``tests/regrtest`` fixtures, or an explicit ``--testdir``.
"""

import os
import sys

# Tests we never auto-discover (they hang, need a tty, or only make
# sense as helpers). Mirrors CPython's STDTESTS/NOTTESTS spirit.
SKIP_BASENAMES = frozenset({
    "__init__",
    "__main__",
})


def findtestdir(path=None):
    """Resolve the directory that holds the ``test_*.py`` modules."""
    if path:
        return os.path.abspath(path)

    env = os.environ.get("WEAVEPY_CPYTHON_LIB")
    if env and os.path.isdir(env):
        return os.path.abspath(env)

    cwd = os.getcwd()
    candidates = [
        os.path.join(cwd, "vendor", "cpython", "Lib", "test"),
        os.path.join(cwd, "vendor", "cpython-tests"),
        os.path.join(cwd, "tests", "regrtest"),
    ]
    for cand in candidates:
        if os.path.isdir(cand):
            return cand

    # Fall back to the directory of the `test` package itself.
    return cwd


def findtests(testdir=None, exclude=()):
    """Return the sorted list of discovered ``test_*`` module names."""
    testdir = findtestdir(testdir)
    names = []
    try:
        entries = os.listdir(testdir)
    except OSError:
        entries = []
    for name in entries:
        if not name.startswith("test_"):
            continue
        if not name.endswith(".py"):
            continue
        modname = name[:-3]
        if modname in SKIP_BASENAMES or modname in exclude:
            continue
        names.append(modname)
    names.sort()
    return names


def split_test_packages(tests):
    """Pass-through hook (CPython splits package tests; we keep names)."""
    return list(tests)
