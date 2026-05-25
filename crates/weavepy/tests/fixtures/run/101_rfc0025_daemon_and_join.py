"""RFC 0025 — daemon flag, join with timeout, and `_shutdown` hook.

Covers four cases that previously short-circuited under the
cooperative model:

1. `daemon=True` is observable to the worker.
2. Setting `daemon` after `start()` raises.
3. `join(timeout=…)` returns even if the worker is still running.
4. `_shutdown()` joins all non-daemon threads.
"""

import threading
import time


# 1 — Worker sees its own daemon flag.
seen = []


def report_daemon():
    me = threading.current_thread()
    seen.append((me.name, me.daemon))


t = threading.Thread(target=report_daemon, name="reporter", daemon=True)
t.start()
t.join()
print("worker saw:", seen)


# 2 — Setting daemon after start is an error.
def short():
    pass


live = threading.Thread(target=short)
live.start()
try:
    live.daemon = True
    print("daemon-after-start missed!")
except RuntimeError:
    print("daemon-after-start raised")
live.join()


# 3 — `join(timeout)` returns False-ish without blocking forever.
go = threading.Event()


def hold():
    go.wait()


h = threading.Thread(target=hold)
h.start()
# Tiny timeout — should return without joining.
h.join(0.01)
print("alive after short join:", h.is_alive())
go.set()
h.join()
print("alive after final join:", h.is_alive())


# 4 — `_shutdown()` waits for non-daemon worker.
finish = threading.Event()
ran = []


def slow_worker():
    finish.wait()
    ran.append("done")


sw = threading.Thread(target=slow_worker)
sw.start()
# Hand the worker a moment, then release it and run shutdown.
time.sleep(0.01)
finish.set()
threading._shutdown()
print("shutdown ran:", ran)
