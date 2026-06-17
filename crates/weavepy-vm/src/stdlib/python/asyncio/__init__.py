"""The asyncio package, tracking PEP 3156."""

# flake8: noqa

import sys

# This relies on each of the submodules having an __all__ variable.
from .base_events import *
from .coroutines import *
from .events import *
from .exceptions import *
from .futures import *
from .locks import *
from .protocols import *
from .runners import *
from .queues import *
from .streams import *
from .subprocess import *
from .tasks import *
from .taskgroups import *
from .timeouts import *
from .threads import *
from .transports import *

__all__ = (base_events.__all__ +
           coroutines.__all__ +
           events.__all__ +
           exceptions.__all__ +
           futures.__all__ +
           locks.__all__ +
           protocols.__all__ +
           runners.__all__ +
           queues.__all__ +
           streams.__all__ +
           subprocess.__all__ +
           tasks.__all__ +
           taskgroups.__all__ +
           threads.__all__ +
           timeouts.__all__ +
           transports.__all__)

if sys.platform == 'win32':  # pragma: no cover
    # WeavePy has no IOCP backend on Windows: the `_overlapped` / `_winapi` /
    # `msvcrt` extensions that `windows_events` (the Proactor loop) needs
    # aren't provided, and the selector backend is gated on a Unix `select(2)`
    # adapter. Degrade gracefully so `import asyncio` still succeeds — which
    # keeps transitive importers like `unittest` working on Windows — even
    # though constructing/running an event loop there isn't supported yet.
    try:
        from .windows_events import *
        __all__ += windows_events.__all__
    except ImportError:
        pass
else:
    from .unix_events import *  # pragma: no cover
    __all__ += unix_events.__all__
