"""A native object's dunder methods, called by name (as `threading.Condition`
calls its lock's `__enter__` and `__exit__`), are the same methods the
protocols use, whether the site is hot or not, and an instance attribute or
a subclass still takes precedence where it does for any method."""

import queue
import threading


def use_by_name(lock, n):
    held = 0
    for _ in range(n):
        lock.__enter__()
        held += 1
        lock.__exit__(None, None, None)
    return held


for make in (threading.Lock, threading.RLock):
    lock = make()
    assert use_by_name(lock, 20000) == 20000
    assert lock.acquire(False)
    lock.release()
    enter = lock.__enter__
    assert enter.__self__ is lock and enter() in (True, None, lock)
    assert not lock.acquire(False) or make is threading.RLock
    if make is threading.RLock:
        lock.release()
    lock.__exit__(None, None, None)
    assert lock.acquire(False)
    lock.release()

cond = threading.Condition()
count = 0
for _ in range(20000):
    with cond:
        count += 1
        cond.notify()
assert count == 20000
try:
    cond.notify()
except RuntimeError:
    pass
else:
    raise AssertionError("notify() without the lock")

ev = threading.Event()
for _ in range(5000):
    ev.set()
    assert ev.is_set()
    ev.clear()
    assert not ev.is_set()

q = queue.Queue()
for i in range(5000):
    q.put(i)
    assert q.get() == i
assert q.empty()


# Dunders of other receivers keep their own rules.
class Sub(dict):
    def __len__(self):
        return 42


s = Sub(a=1)
for _ in range(3000):
    assert s.__len__() == 42 and dict.__len__(s) == 1 and len(s) == 42


class Base:
    def __repr__(self):
        return "base"


class Child(Base):
    def __repr__(self):
        return "child:" + super().__repr__()

    def __init__(self):
        super().__init__()


c = Child()
for _ in range(3000):
    assert c.__repr__() == "child:base"

e = ValueError("x")
for _ in range(3000):
    assert e.__str__() == "x" and e.__reduce__()[0] is ValueError
print("ok")
