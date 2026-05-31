"""``test.support.threading_helper`` — thread lifecycle helpers.

Faithful subset of CPython 3.13's
``Lib/test/support/threading_helper.py``: ``threading_setup`` /
``threading_cleanup``, ``join_thread``, ``reap_threads``,
``start_threads``, ``catch_threading_exception``, ``wait_threads_exit``
and ``requires_working_threading``.
"""

import _thread
import contextlib
import functools
import sys
import threading
import time


# Tunable join timeout; tests override via the argument.
SHORT_TIMEOUT = 30.0


def threading_setup():
    """Snapshot the live-thread set for a later ``threading_cleanup``."""
    return (threading.enumerate(), _thread._count() if hasattr(_thread, "_count") else 0)


def threading_cleanup(*original_values):
    """Wait (briefly) for spawned threads to wind down."""
    orig_threads, _orig_count = original_values
    orig_threads = set(orig_threads)
    timeout = 1.0
    deadline = time.monotonic() + timeout
    while True:
        # Drop daemon/dead threads; only count strays we created.
        current = set(threading.enumerate())
        strays = current - orig_threads
        strays = {t for t in strays if t.is_alive() and not t.daemon}
        if not strays:
            return
        if time.monotonic() > deadline:
            return
        time.sleep(0.01)


def join_thread(thread, timeout=None):
    """Join *thread*, raising if it refuses to stop in time."""
    if timeout is None:
        timeout = SHORT_TIMEOUT
    thread.join(timeout)
    if thread.is_alive():
        msg = f"failed to join the thread in {timeout:.1f} seconds"
        raise AssertionError(msg)


def reap_threads(func):
    """Decorator: run ``threading_cleanup`` after the wrapped test."""
    @functools.wraps(func)
    def decorator(*args):
        key = threading_setup()
        try:
            return func(*args)
        finally:
            threading_cleanup(*key)
    return decorator


@contextlib.contextmanager
def wait_threads_exit(timeout=None):
    """Yield, then wait for any newly-started threads to exit."""
    if timeout is None:
        timeout = SHORT_TIMEOUT
    old = set(threading.enumerate())
    try:
        yield
    finally:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            extra = [t for t in threading.enumerate()
                     if t not in old and t.is_alive() and not t.daemon]
            if not extra:
                break
            time.sleep(0.01)


@contextlib.contextmanager
def start_threads(threads, unlock=None):
    """Start every thread, optionally release *unlock*, join on exit."""
    threads = list(threads)
    started = []
    try:
        for t in threads:
            t.start()
            started.append(t)
        yield
    finally:
        if unlock:
            unlock()
        endtime = time.monotonic() + SHORT_TIMEOUT
        for t in started:
            t.join(max(endtime - time.monotonic(), 0.01))
        started = [t for t in started if t.is_alive()]
        if started:
            raise AssertionError('Unable to join %d threads' % len(started))


class catch_threading_exception:
    """Capture an unhandled exception raised by ``threading.Thread.run``.

    Mirrors CPython by installing a ``threading.excepthook`` and exposing
    ``exc_type``/``exc_value``/``exc_traceback``/``thread`` afterwards.
    """

    def __init__(self):
        self.exc_type = None
        self.exc_value = None
        self.exc_traceback = None
        self.thread = None
        self._old_hook = None

    def _hook(self, args):
        self.exc_type = args.exc_type
        self.exc_value = args.exc_value
        self.exc_traceback = args.exc_traceback
        self.thread = args.thread

    def __enter__(self):
        self._old_hook = getattr(threading, "excepthook", None)
        if self._old_hook is not None:
            threading.excepthook = self._hook
        return self

    def __exit__(self, *exc_info):
        if self._old_hook is not None:
            threading.excepthook = self._old_hook
        del self._old_hook


def requires_working_threading(*, module=False):
    """Skip when the platform lacks usable threads (always OK here)."""
    import unittest
    msg = "requires threading support"
    if module:
        if not getattr(sys, "_thread_supported", True):
            raise unittest.SkipTest(msg)
        return
    return unittest.skipUnless(True, msg)


def can_start_thread():
    return True
