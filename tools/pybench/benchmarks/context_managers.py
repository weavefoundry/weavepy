"""`with` statements: class-based managers, contextlib.contextmanager,
suppress, ExitStack, and threading locks."""

import contextlib
import threading

WORK = 40000


class Tracker:
    def __init__(self):
        self.depth = 0
        self.peak = 0

    def __enter__(self):
        self.depth += 1
        if self.depth > self.peak:
            self.peak = self.depth
        return self

    def __exit__(self, exc_type, exc, tb):
        self.depth -= 1
        return False


@contextlib.contextmanager
def tagged(log, tag):
    log.append(tag)
    try:
        yield len(log)
    finally:
        log.pop()


def bench(n):
    t = Tracker()
    lock = threading.Lock()
    rlock = threading.RLock()
    log = []
    total = 0
    for i in range(n):
        with t, lock:
            with t:
                total += t.depth
        with rlock:
            with rlock:
                total += 1
        with tagged(log, i) as k:
            total += k
        with contextlib.suppress(KeyError):
            {}[i]
        if i % 8 == 0:
            with contextlib.ExitStack() as stack:
                stack.enter_context(t)
                stack.callback(log.clear)
                total += t.depth
    return (total, t.peak)
