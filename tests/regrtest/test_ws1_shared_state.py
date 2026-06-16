"""RFC 0039 WS1 — shared interpreter state across worker threads.

The worker model shares (not forks) the process-wide tables: `sys.modules`,
the `builtins` dict, and the `threading` registry. So a module imported or
monkey-patched on one thread is visible on another, and the thread registry
is identity-consistent across threads. This pins those invariants in-process
(no CPython checkout needed).
"""

import sys
import threading


# ---------------------------------------------------------------------------
# A module imported on the main thread is visible in the worker, and a module
# imported *inside* the worker is visible to the main thread after join — both
# threads read the same shared `sys.modules`.
# ---------------------------------------------------------------------------

import math  # noqa: E402  (import-after-statement is intentional)

worker_saw_math = []
imported_in_worker = []


def importer():
    worker_saw_math.append("math" in sys.modules)
    worker_saw_math.append(sys.modules["math"] is math)
    import base64  # first import happens on the worker thread

    imported_in_worker.append(base64.b64encode(b"hi"))


t = threading.Thread(target=importer)
t.start()
t.join()

assert worker_saw_math == [True, True], worker_saw_math
assert "base64" in sys.modules, "worker import must populate the shared module table"
assert imported_in_worker == [b"aGk="], imported_in_worker


# ---------------------------------------------------------------------------
# Monkey-patching a shared module attribute on a worker is visible to the
# parent (same module object, not a per-thread copy).
# ---------------------------------------------------------------------------

math_sentinel = object()


def patcher():
    math._ws1_sentinel = math_sentinel


p = threading.Thread(target=patcher)
p.start()
p.join()

assert getattr(math, "_ws1_sentinel", None) is math_sentinel
del math._ws1_sentinel


# ---------------------------------------------------------------------------
# A mutable object created on the parent is shared by reference: appends from
# several workers are all observable after join (the heap is `Arc`-rooted).
# ---------------------------------------------------------------------------

shared = []
shared_lock = threading.Lock()


def appender(value):
    with shared_lock:
        shared.append(value)


workers = [threading.Thread(target=appender, args=(i,)) for i in range(6)]
for w in workers:
    w.start()
for w in workers:
    w.join()
assert sorted(shared) == [0, 1, 2, 3, 4, 5], shared


# ---------------------------------------------------------------------------
# The threading registry is identity-consistent across threads: the worker
# observes the *same* main-thread identity the parent does.
# ---------------------------------------------------------------------------

main_ident = threading.main_thread().ident
worker_view = {}


def observe():
    worker_view["main_ident"] = threading.main_thread().ident
    worker_view["main_is_parent"] = threading.main_thread() is parent_main
    worker_view["self_in_enumerate"] = threading.current_thread() in threading.enumerate()


parent_main = threading.main_thread()
o = threading.Thread(target=observe)
o.start()
o.join()

assert worker_view["main_ident"] == main_ident, worker_view
assert worker_view["main_is_parent"] is True, worker_view
assert worker_view["self_in_enumerate"] is True, worker_view


print("WS1 shared interpreter state ok")
