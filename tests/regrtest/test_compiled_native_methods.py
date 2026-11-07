"""Compiled code's direct calls of an instance's native methods (a
`collections.deque`'s, an asyncio future's) keep CPython's results: the
values, an exception the method raises (with its line), a method an
instance attribute shadows, a class whose method is replaced, a subclass
overriding the method, and an argument that isn't bound yet."""
import asyncio
import collections
import sys


def churn(q, n):
    total = 0
    for i in range(n):
        q.append(i)
        q.appendleft(-i)
        total += q.pop() - q.popleft()
    return total


def drain(q):
    try:
        q.popleft()
    except IndexError as e:
        return (str(e), sys.exc_info()[2].tb_lineno)
    return None


q = collections.deque()
for _ in range(3):
    assert churn(q, 3000) == sum(2 * i for i in range(3000))
assert len(q) == 0
for _ in range(3000):
    r = drain(q)
assert r == ("pop from an empty deque", drain.__code__.co_firstlineno + 2), r


class Shadowed(collections.deque):
    pass


def push(q, v):
    q.append(v)
    return len(q)


s = Shadowed()
for i in range(3000):
    push(s, i)
s.append = lambda v: None
assert push(s, 1) == 3000


class Loud(collections.deque):
    def append(self, v):
        collections.deque.append(self, v * 2)


lq = Loud()
for i in range(3000):
    push(lq, i)
assert lq[-1] == 2 * 2999


class Swapped(collections.deque):
    pass


w = Swapped()
for i in range(3000):
    push(w, i)
Swapped.append = lambda self, v: collections.deque.append(self, -1)
push(w, 7)
assert w[-1] == -1


def maybe(q, flag):
    if flag:
        v = 5
    q.append(v)


m = collections.deque()
for _ in range(3000):
    maybe(m, True)
try:
    maybe(m, False)
except UnboundLocalError:
    pass
else:
    raise AssertionError("no UnboundLocalError")


async def futures(n):
    loop = asyncio.get_running_loop()
    done = 0
    for i in range(n):
        f = loop.create_future()
        f.set_result(i)
        done += f.done() and f.result() == i and not f.cancelled()
    f = loop.create_future()
    try:
        f.result()
    except asyncio.InvalidStateError:
        done += 1
    return done


assert asyncio.run(futures(3000)) == 3001
print("ok")
