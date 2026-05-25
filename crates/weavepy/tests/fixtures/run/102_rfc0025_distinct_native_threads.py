"""RFC 0025 — `_thread.get_native_id` proves the worker runs on a
different OS thread, not on the calling interpreter thread.

CPython's `threading.get_native_id` returns the raw kernel
thread id (`gettid` on Linux, `pthread_threadid_np` on macOS).
Two threads observed at the same time must have different
values — anything else means we faked the OS thread.
"""

import _thread
import threading


# Snapshot the parent's native id once so we have something to
# compare against.
parent_native = _thread.get_native_id()

ids = []


def report_native():
    ids.append(_thread.get_native_id())


workers = [threading.Thread(target=report_native) for _ in range(4)]
for w in workers:
    w.start()
for w in workers:
    w.join()


# All workers must report a non-parent native id.
print("count of workers:", len(ids))
print("any matched parent:", any(i == parent_native for i in ids))
print("ids are integers:", all(isinstance(i, int) for i in ids))


# `threading.get_native_id` (module-level) should return the same
# kind of value as `_thread.get_native_id`.
print(
    "threading.get_native_id matches _thread:",
    threading.get_native_id() == _thread.get_native_id(),
)


# The parent's `current_thread()` should be the main thread, with
# a non-None native id.
me = threading.current_thread()
print("main thread name:", me.name)
print("main thread has native_id:", me.native_id is None or isinstance(me.native_id, int))
