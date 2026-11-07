"""`collections.deque` lays its slots out like any slotted class: its
native methods keep working on plain deques and on subclasses (with
`__slots__`, with a `__dict__`), and attribute assignment on a plain deque
still fails as CPython's does."""
from collections import deque
import copy
import pickle
import weakref


def run(i):
    d = deque([1, 2, 3], maxlen=5)
    d.append(4)
    d.appendleft(0)
    d.append(5)
    assert list(d) == [1, 2, 3, 4, 5]
    assert d.popleft() == 1 and d.pop() == 5 and d.maxlen == 5
    d.extend(range(10))
    assert list(d) == [5, 6, 7, 8, 9]
    d.rotate(2)
    assert list(d) == [8, 9, 5, 6, 7]
    it = iter(d)
    next(it)
    d.append(i)
    try:
        next(it)
    except RuntimeError:
        pass
    else:
        raise AssertionError("mutation during iteration not seen")
    assert pickle.loads(pickle.dumps(d)) == d == copy.copy(d) == copy.deepcopy(d)
    assert weakref.ref(d)() is d
    q = deque()
    for k in range(100):
        q.append(k)
    for k in range(60):
        q.popleft()
    assert len(q) == 40 and q[0] == 60 and q[-1] == 99 and q.index(70) == 10


class Slotted(deque):
    __slots__ = ("x", "__dict__")


class Plain(deque):
    pass


for i in range(3000):
    run(i)

try:
    deque().foo = 1
except AttributeError:
    pass
else:
    raise AssertionError("a plain deque took an attribute")
s = Slotted([1, 2])
s.x = 3
s.y = 4
s.append(5)
assert (list(s), s.x, s.y, s.popleft()) == ([1, 2, 5], 3, 4, 1)
p = Plain("abc")
p.z = 1
p.appendleft("q")
assert (list(p), p.z, p[1], p[-1], list(reversed(p))) == (["q", "a", "b", "c"], 1, "a", "c", ["c", "b", "a", "q"])
print("ok")
