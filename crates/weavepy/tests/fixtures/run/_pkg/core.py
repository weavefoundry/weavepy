"""Inner module of `_pkg` — referenced by `__init__.py` and tests."""

GREETING = "hello from _pkg.core"


def greet(name):
    return GREETING + " to " + name
