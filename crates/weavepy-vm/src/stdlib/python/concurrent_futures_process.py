"""WeavePy stub for `concurrent.futures.process`.

WeavePy has no `multiprocessing` runtime, so a real process pool cannot
be provided. The name is still importable (so `concurrent.futures`'
lazy `__getattr__` and `from concurrent.futures import *` work), but
constructing a `ProcessPoolExecutor` raises, matching the spirit of
CPython on a platform where multiprocessing is unavailable.
"""

from concurrent.futures._base import Executor, BrokenExecutor


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
