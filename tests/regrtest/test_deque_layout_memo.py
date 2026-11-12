"""A deque's native methods remember the slot layout they found led by its
names: a plain deque, a subclass adding slots and one adding a dict, used
in turn, each keep their own contents through appends, pops, indexing,
rotation, truth tests, len() and iteration, in compiled loops."""

from collections import deque


class Slotted(deque):
    __slots__ = ("tag",)


class Open(deque):
    pass


def churn(q, n):
    t = 0
    for i in range(n):
        q.append(i)
        q.appendleft(-i)
        if i & 3 == 3:
            t += q.pop() + q.popleft()
        if i & 7 == 0:
            t += q[0] + q[-1]
    q.rotate(2)
    while len(q) > 4:
        t += q.popleft()
    t += sum(x for x in q)
    return t + bool(q)


def expected(n):
    q = list()
    t = 0
    for i in range(n):
        q.append(i)
        q.insert(0, -i)
        if i & 3 == 3:
            t += q.pop() + q.pop(0)
        if i & 7 == 0:
            t += q[0] + q[-1]
    q = q[-2:] + q[:-2]
    while len(q) > 4:
        t += q.pop(0)
    return t + sum(q) + bool(q)


s = Slotted()
s.tag = "s"
o = Open()
o.extra = 1
for _ in range(3):
    for q in (deque(), s, o, deque(maxlen=None)):
        q.clear()
        assert churn(q, 3000) == expected(3000), type(q)
assert s.tag == "s" and o.extra == 1

ring = deque(maxlen=8)
for i in range(1000):
    ring.append(i)
assert list(ring) == list(range(992, 1000))
assert [x for x in Slotted([1, 2, 3])] == [1, 2, 3]
print("ok")
