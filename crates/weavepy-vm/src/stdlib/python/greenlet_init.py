"""greenlet — lightweight in-process concurrent programming.

WeavePy facade over the native ``_greenlet`` module (RFC 0066 WS4).
Unlike the upstream cp313 wheel — whose hand-written per-platform stack
switching manipulates CPython's C stack and can never work here — the
real machinery lives inside the interpreter: each started greenlet runs
on its own dedicated native stack, and a switch parks the whole stack,
interpreter recursion and C frames included.

The module is version-stringed to the upstream line it models, and a
``greenlet-<version>.dist-info`` ships in the stdlib path so
``importlib.metadata`` (and therefore pip and dependents like
SQLAlchemy's asyncio bridge) see an installed distribution.
"""

from _greenlet import (
    GREENLET_VERSION,
    GreenletExit,
    error,
    getcurrent,
    gettrace,
    greenlet,
    settrace,
)

__version__ = GREENLET_VERSION

# Upstream API-surface aliases.
_C_API = None  # The C-API capsule is a WS6 stretch (gevent); see RFC 0066.

__all__ = [
    "__version__",
    "GreenletExit",
    "error",
    "getcurrent",
    "gettrace",
    "greenlet",
    "settrace",
]
