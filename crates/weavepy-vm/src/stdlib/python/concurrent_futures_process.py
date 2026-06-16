"""WeavePy stub for `concurrent.futures.process`.

WeavePy has no `multiprocessing` runtime, so a real process pool cannot
be provided. The name is still importable (so `concurrent.futures`'
lazy `__getattr__` and `from concurrent.futures import *` work), but
constructing a `ProcessPoolExecutor` raises, matching the spirit of
CPython on a platform where multiprocessing is unavailable.
"""

from concurrent.futures._base import Executor, BrokenExecutor

# EXTRA_QUEUED_CALLS mirrors CPython's process.py constant; some callers
# (and `from concurrent.futures.process import *`) reference it.
EXTRA_QUEUED_CALLS = 1

_system_limits_checked = False
_system_limited = None


def _check_system_limits():
    """CPython exposes this so callers can detect platforms where a real
    process pool can't be built. WeavePy has no multiprocessing process
    runtime, so we behave exactly like such a platform: raise
    ``NotImplementedError``. `test.test_concurrent_futures` keys its
    ProcessPool skips off this, so the ThreadPool suite still runs.
    """
    global _system_limits_checked, _system_limited
    if _system_limits_checked:
        if _system_limited:
            raise NotImplementedError(_system_limited)
        return
    _system_limits_checked = True
    _system_limited = (
        "ProcessPoolExecutor is unavailable: WeavePy has no multiprocessing "
        "process runtime (RFC 0026 ships threads/queues, not a fork/spawn "
        "worker pool)."
    )
    raise NotImplementedError(_system_limited)


class BrokenProcessPool(BrokenExecutor):
    """Raised when a process in a ProcessPoolExecutor terminated abruptly."""


class ProcessPoolExecutor(Executor):
    def __init__(self, max_workers=None, mp_context=None,
                 initializer=None, initargs=(), *, max_tasks_per_child=None):
        raise NotImplementedError(
            "ProcessPoolExecutor is not supported: WeavePy has no "
            "multiprocessing runtime. Use ThreadPoolExecutor instead."
        )


__all__ = ["ProcessPoolExecutor", "BrokenProcessPool"]
