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

import sys as _sys

import _greenlet
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

# Upstream layout parity (RFC 0072 WS1): the C extension lives at
# ``greenlet._greenlet``, and C consumers may import the capsule through
# either dotted path. Alias the native module under the package so
# ``PyCapsule_Import("greenlet._greenlet._C_API", 0)`` resolves.
_sys.modules.setdefault("greenlet._greenlet", _greenlet)

# The C-API capsule (RFC 0072 WS1). The real ``PyGreenlet_*`` table is
# minted C-side on first import — ``PyCapsule_Import("greenlet._C_API",
# 0)`` installs it over this placeholder (the RFC 0057 `datetime_CAPI`
# stand-in discipline).
_C_API = None

__all__ = [
    "__version__",
    "GreenletExit",
    "error",
    "getcurrent",
    "gettrace",
    "greenlet",
    "settrace",
]
