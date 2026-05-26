"""ProcessPoolExecutor implementation built on top of WeavePy's real
``multiprocessing`` package.

This module is imported by ``concurrent.futures`` and provides the
``ProcessPoolExecutor`` symbol that previously lived as a stub.
"""

from __future__ import annotations

import os
import sys
import threading
import weakref

try:
    import multiprocessing as _mp
except Exception as _exc:  # pragma: no cover — multiprocessing must exist
    raise RuntimeError("multiprocessing unavailable in this WeavePy build") from _exc


class _CallItem:
    __slots__ = ("future", "fn", "args", "kwargs")

    def __init__(self, future, fn, args, kwargs):
        self.future = future
        self.fn = fn
        self.args = args
        self.kwargs = kwargs


class _ResultItem:
    __slots__ = ("future", "result", "exception")

    def __init__(self, future, result=None, exception=None):
        self.future = future
        self.result = result
        self.exception = exception


def _worker_loop(call_queue, result_queue):
    while True:
        item = call_queue.get()
        if item is None:
            return
        future, fn, args, kwargs = item
        try:
            result = fn(*args, **kwargs)
            result_queue.put((future, result, None))
        except BaseException as exc:
            result_queue.put((future, None, exc))


class ProcessPoolExecutor:
    """Subset of :class:`concurrent.futures.ProcessPoolExecutor`.

    Limitations vs. CPython:
    - ``initializer``/``initargs`` are honoured but called once per
      worker rather than via a separate broker process.
    - ``mp_context`` selects only the start method; custom contexts
      are not supported beyond what ``multiprocessing.get_context``
      returns.
    """

    def __init__(self, max_workers=None, mp_context=None,
                 initializer=None, initargs=()):
        from concurrent.futures import Future
        self._Future = Future
        if max_workers is None:
            max_workers = os.cpu_count() or 1
        if max_workers <= 0:
            raise ValueError("max_workers must be >= 1")
        self._max_workers = max_workers
        self._ctx = mp_context or _mp.get_context()
        self._initializer = initializer
        self._initargs = tuple(initargs)
        self._call_queue = self._ctx.Queue()
        self._result_queue = self._ctx.Queue()
        self._pending = {}
        self._processes = []
        self._lock = threading.Lock()
        self._shutdown = False
        self._dispatcher = threading.Thread(target=self._dispatch_results, daemon=True)
        self._dispatcher.start()
        for _ in range(max_workers):
            self._spawn_worker()

    def _spawn_worker(self):
        p = self._ctx.Process(
            target=_worker_entry,
            args=(self._call_queue, self._result_queue, self._initializer, self._initargs),
        )
        p.daemon = True
        p.start()
        self._processes.append(p)

    def _dispatch_results(self):
        while True:
            item = self._result_queue.get()
            if item is None:
                return
            future_id, result, exception = item
            with self._lock:
                future = self._pending.pop(future_id, None)
            if future is None:
                continue
            if exception is not None:
                future.set_exception(exception)
            else:
                future.set_result(result)

    def submit(self, fn, /, *args, **kwargs):
        if self._shutdown:
            raise RuntimeError("cannot schedule on a shut-down ProcessPoolExecutor")
        future = self._Future()
        with self._lock:
            future_id = id(future)
            self._pending[future_id] = future
        self._call_queue.put((future_id, fn, args, kwargs))
        return future

    def map(self, fn, *iterables, timeout=None, chunksize=1):
        if chunksize < 1:
            raise ValueError("chunksize must be >= 1")
        futures = [self.submit(fn, *args) for args in zip(*iterables)]
        for f in futures:
            yield f.result(timeout)

    def shutdown(self, wait=True, *, cancel_futures=False):
        with self._lock:
            if self._shutdown:
                return
            self._shutdown = True
        for _ in self._processes:
            try:
                self._call_queue.put(None)
            except Exception:
                pass
        if cancel_futures:
            with self._lock:
                pending = list(self._pending.values())
                self._pending.clear()
            for f in pending:
                f.cancel()
        if wait:
            for p in self._processes:
                try:
                    p.join()
                except Exception:
                    pass
        try:
            self._result_queue.put(None)
        except Exception:
            pass

    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc, tb):
        self.shutdown(wait=True)
        return False


def _worker_entry(call_queue, result_queue, initializer, initargs):
    if initializer is not None:
        try:
            initializer(*initargs)
        except BaseException:
            return
    while True:
        item = call_queue.get()
        if item is None:
            return
        future_id, fn, args, kwargs = item
        try:
            result = fn(*args, **kwargs)
            result_queue.put((future_id, result, None))
        except BaseException as exc:
            result_queue.put((future_id, None, exc))


__all__ = ["ProcessPoolExecutor"]
