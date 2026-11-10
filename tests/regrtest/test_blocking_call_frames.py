"""A thread blocked in a native call (a `select`, a `time.sleep`, a lock)
from compiled code shows its whole Python stack to whatever can look while
it blocks: another thread's `sys._current_frames()` and faulthandler's
dumps. Single-threaded, the stack seen afterwards is whole too."""

import faulthandler
import os
import selectors
import sys
import tempfile
import threading
import time


def innermost(seconds):
    time.sleep(seconds)


def middle(seconds):
    innermost(seconds)
    return 1


def outer(seconds):
    total = 0
    for _ in range(3):
        total += middle(seconds)
    return total


# Warm the chain so it runs compiled.
for _ in range(300):
    outer(0)


def names_of(frame):
    out = []
    while frame is not None:
        out.append(frame.f_code.co_name)
        frame = frame.f_back
    return out


# Another thread looks while the main thread sleeps.
seen = []
main_id = threading.get_ident()


def watcher():
    time.sleep(0.05)
    frame = sys._current_frames().get(main_id)
    seen.append(names_of(frame))


t = threading.Thread(target=watcher)
t.start()
outer(0.1)
t.join()
assert seen, "watcher saw nothing"
stack = seen[0]
assert stack[:3] == ["innermost", "middle", "outer"], stack


# faulthandler's watchdog dumps the sleeping stack.
with tempfile.TemporaryFile("w+") as f:
    faulthandler.dump_traceback_later(0.05, file=f)
    try:
        outer(0.1)
    finally:
        faulthandler.cancel_dump_traceback_later()
    f.seek(0)
    dump = f.read()
assert "innermost" in dump and "middle" in dump and "outer" in dump, dump


# Single-threaded: a blocking select from compiled code, then a stack walk.
sel = selectors.DefaultSelector()
r, w = os.pipe()
sel.register(r, selectors.EVENT_READ)


def poll_once():
    return sel.select(0)


def poll_loop(n):
    count = 0
    for _ in range(n):
        count += len(poll_once())
    return names_of(sys._getframe())


for _ in range(3):
    assert poll_loop(500)[0] == "poll_loop"
os.write(w, b"x")
assert len(poll_once()) == 1
sel.close()
os.close(r)
os.close(w)
print("ok")
