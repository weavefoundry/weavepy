"""Compiled loops over deques keep exact results, errors, and fallbacks.

Hot loops call `deque` methods, subscripts, `len`, truth tests, and
iteration natively from compiled code. Each check below runs long enough
to compile and then changes the shape (a subclass override, a non-integer
element, a mutation during iteration) so the compiled code must fall back
without changing what Python observes.
"""

from collections import deque
import gc
import pickle
import weakref

N = 3000


def fifo(n):
    total = 0
    q = deque()
    for i in range(n):
        q.append(i)
        q.append(i + 1)
        total += q.popleft()
        if i & 7 == 0:
            total += q[0] + q[-1]
    while q:
        total += q.popleft()
    return total


def fifo_list(n):
    total = 0
    q = []
    for i in range(n):
        q.append(i)
        q.append(i + 1)
        total += q.pop(0)
        if i & 7 == 0:
            total += q[0] + q[-1]
    while q:
        total += q.pop(0)
    return total


assert fifo(N) == fifo_list(N)


def ring(n):
    total = 0
    r = deque(maxlen=64)
    for i in range(n):
        r.append(i)
        if i & 15 == 15:
            r.rotate(3)
            total += r[5]
    return total + sum(r), list(r)


def ring_list(n):
    total = 0
    r = []
    for i in range(n):
        r.append(i)
        if len(r) > 64:
            del r[0]
        if i & 15 == 15:
            r[:] = r[-3:] + r[:-3]
            total += r[5]
    return total + sum(r), r


assert ring(N) == ring_list(N)


def stack(n):
    total = 0
    s = deque()
    for i in range(n):
        s.appendleft(i)
        if i & 3 == 3:
            total += s.pop() + s.popleft()
    total += len(s)
    for x in s:
        total += x
    return total


def stack_list(n):
    total = 0
    s = []
    for i in range(n):
        s.insert(0, i)
        if i & 3 == 3:
            total += s.pop() + s.pop(0)
    total += len(s)
    for x in s:
        total += x
    return total


assert stack(N) == stack_list(N)


# Elements that are not integers: the integer speculation misses and the
# exact generic operation runs instead.
def concat(q, n):
    out = None
    for _ in range(n):
        out = q[0] + q[-1]
    return out


assert concat(deque([1, 2]), N) == 3
assert concat(deque(["a", "b"]), N) == "ab"
assert concat(deque([1.5, 2]), 3) == 3.5


def pops(q, n):
    total = 0
    for i in range(n):
        q.append(i)
        total += q.pop()
    return total


assert pops(deque(), N) == sum(range(N))
# A non-integer element below the integers is never popped.
q = deque(["x"])
assert pops(q, N) == sum(range(N))
assert list(q) == ["x"]


# Errors raised by a native method inside a compiled loop.
def drain(q, n):
    caught = 0
    for _ in range(n):
        try:
            q.popleft()
        except IndexError as e:
            assert str(e) == "pop from an empty deque", e
            caught += 1
    return caught


assert drain(deque(range(10)), N) == N - 10


def peek(q, i, n):
    total = 0
    for _ in range(n):
        total += q[i]
    return total


assert peek(deque([5, 6, 7]), -1, N) == 7 * N
try:
    peek(deque([5]), 3, 2)
except IndexError as e:
    assert str(e) == "deque index out of range", e
else:
    raise AssertionError("IndexError expected")


# Subclasses that override the operations keep their overrides.
class Doubling(deque):
    def __getitem__(self, i):
        return 2 * deque.__getitem__(self, i)

    def __len__(self):
        return 100

    def __iter__(self):
        yield from (2 * x for x in deque.__iter__(self))


class Scaling(deque):
    def append(self, x):
        deque.append(self, x * 10)


d = Doubling([1, 2, 3])
assert peek(d, 0, N) == 2 * N


def lengths(q, n):
    total = 0
    for _ in range(n):
        total += len(q)
    return total


assert lengths(deque([1, 2]), N) == 2 * N
assert lengths(d, 3) == 300


def appends(q, n):
    for i in range(n):
        q.append(i)
    return q


assert list(appends(deque(), 5)) == [0, 1, 2, 3, 4]
assert list(appends(Scaling(), 3)) == [0, 10, 20]


def iterate(q):
    total = 0
    for x in q:
        total += x
    return total


assert iterate(deque(range(N))) == sum(range(N))
assert iterate(Doubling([1, 2])) == 6

# Mutation during compiled iteration raises exactly.
def mutate(q):
    seen = 0
    for x in q:
        seen += 1
        if seen == 5:
            q.append(x)
    return seen


try:
    mutate(deque(range(N)))
except RuntimeError as e:
    assert str(e) == "deque mutated during iteration", e
else:
    raise AssertionError("RuntimeError expected")


# Native iterators keep the iterator classes' surface.
q = deque([1, 2, 3])
it = iter(q)
assert type(it).__name__ == "_deque_iterator", type(it)
assert next(it) == 1
assert list(pickle.loads(pickle.dumps(it))) == [2, 3]
assert list(it) == [2, 3]
rit = reversed(q)
assert type(rit).__name__ == "_deque_reverse_iterator", type(rit)
assert list(rit) == [3, 2, 1]
assert it.__length_hint__() == 0
assert iter(q).__length_hint__() == 3


# A deque holding its own iterator is collectable.
q = deque()
q.append(iter(q))
ref = weakref.ref(q)
del q
gc.collect()
assert ref() is None

print("Compiled deque lanes: ok")
