"""RFC 0025 — cross-thread heap sharing regression test.

Worker threads spawned via `threading.Thread` must:

1. Run on a real OS thread (different native id from the parent).
2. See the same heap as the parent — captured closures over lists,
   dicts, sets etc. are shared.
3. Route exceptions through the *thread's own* `sys.exc_info()`,
   so `threading.excepthook` receives the correct exception
   (regression: under the cooperative model `sys.exc_info()` read
   the parent's exception stack and surfaced bogus errors to the
   hook).
4. Honor the `daemon=True` flag.
5. Release `_tstate_lock` cleanly so `join()` returns once the
   worker exits.
"""

import _thread
import sys
import threading
import time


# 1 — Real OS thread: native id differs from the parent.
parent_native = _thread.get_native_id()
seen_native = []


def report_native():
    seen_native.append(_thread.get_native_id())


t = threading.Thread(target=report_native)
t.start()
t.join()
assert len(seen_native) == 1
assert seen_native[0] != parent_native, (seen_native[0], parent_native)


# 2 — Shared mutable list.
shared = []


def append_all(items):
    for x in items:
        shared.append(x)


w = threading.Thread(target=append_all, args=([1, 2, 3, 4, 5],))
w.start()
w.join()
assert shared == [1, 2, 3, 4, 5], shared

# Shared dict mutation.
shared_dict = {}


def fill_dict():
    for i in range(5):
        shared_dict[i] = i * i


w = threading.Thread(target=fill_dict)
w.start()
w.join()
assert shared_dict == {0: 0, 1: 1, 2: 4, 3: 9, 4: 16}, shared_dict


# 3 — Excepthook gets the right exception (regression test for
# RFC 0025: workers used to read the parent's `exc_info_stack`).
seen_hook = []
saved = threading.excepthook


def my_hook(args):
    seen_hook.append((args.exc_type.__name__, str(args.exc_value)))


threading.excepthook = my_hook


def boom():
    raise ValueError("crossed-the-streams")


w = threading.Thread(target=boom)
w.start()
w.join()
threading.excepthook = saved
assert seen_hook == [("ValueError", "crossed-the-streams")], seen_hook


# 4 — daemon=True propagates to the worker.
saw_daemon = []


def report_daemon():
    saw_daemon.append(threading.current_thread().daemon)


d = threading.Thread(target=report_daemon, daemon=True)
d.start()
d.join()
assert saw_daemon == [True], saw_daemon


# 5 — `join` with timeout doesn't deadlock when the worker is
# already finished.
def quick():
    pass


q = threading.Thread(target=quick)
q.start()
q.join(timeout=5.0)
assert not q.is_alive()


# 6 — `Event` round-trip across thread boundary.
go = threading.Event()
done = threading.Event()


def waiter():
    go.wait()
    done.set()


wt = threading.Thread(target=waiter)
wt.start()
time.sleep(0.01)
go.set()
assert done.wait(timeout=2.0)
wt.join()


# 7 — `get_ident()` is stable inside a worker.
own = []


def report():
    own.append(threading.get_ident())
    own.append(threading.get_ident())


r = threading.Thread(target=report)
r.start()
r.join()
assert own[0] == own[1]


# 8 — `sys.exc_info()` inside the worker reflects the worker's own
# exception, even when the parent is mid-handler. This was the
# specific bug the `vm_singletons::current_thread_handles` plumbing
# fixes.
worker_info = []


def nested_raiser():
    try:
        raise KeyError("worker-key")
    except KeyError:
        worker_info.append(sys.exc_info()[0].__name__)


parent_info = []
try:
    raise RuntimeError("parent-rt")
except RuntimeError:
    # Worker raises a *different* exception while the parent is mid
    # handler — without per-thread routing, `worker_info` would
    # observe RuntimeError.
    w = threading.Thread(target=nested_raiser)
    w.start()
    w.join()
    parent_info.append(sys.exc_info()[0].__name__)

assert worker_info == ["KeyError"], worker_info
assert parent_info == ["RuntimeError"], parent_info


print("cross-thread heap ok")
