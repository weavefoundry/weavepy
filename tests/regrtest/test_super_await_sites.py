"""Zero-argument super() method loads and awaited coroutine calls, each run
hot enough that compiled code takes its site caches, with the results the
general protocol gives after the classes change underneath them."""

import asyncio

N = 3000


class A:
    def m(self, x):
        return ("A", x)

    def n(self):
        return "A.n"

    @classmethod
    def c(cls):
        return ("A.c", cls.__name__)


class B(A):
    def m(self, x):
        return ("B", super().m(x))

    def bound(self):
        f = super().n
        return f()

    @classmethod
    def c(cls):
        return ("B.c", super().c())


class C(A):
    def m(self, x):
        return ("C", super().m(x))


class D(B, C):
    def m(self, x):
        return ("D", super().m(x))


def run(o, k=N):
    r = None
    for i in range(k):
        r = o.m(i)
    return r


def bound_loop(o, k=N):
    r = None
    for _ in range(k):
        r = o.bound()
    return r


def check_super():
    for _ in range(3):
        assert run(B()) == ("B", ("A", N - 1))
        assert run(D()) == ("D", ("B", ("C", ("A", N - 1))))
        assert bound_loop(B()) == "A.n"
        assert B.c() == ("B.c", ("A.c", "B"))
    # A base method replaced after the sites warmed.
    A.m = lambda self, x: ("A2", x)
    assert run(B()) == ("B", ("A2", N - 1))
    assert run(D()) == ("D", ("B", ("C", ("A2", N - 1))))

    # A new receiver class at the same sites.
    class F(B):
        pass

    assert run(F()) == ("B", ("A2", N - 1))
    # The receiver's class reassigned.
    b = B()
    assert run(b) == ("B", ("A2", N - 1))
    b.__class__ = D
    assert run(b) == ("D", ("B", ("C", ("A2", N - 1))))
    # A method in between removed.
    del C.m
    assert run(D()) == ("D", ("B", ("A2", N - 1)))

    # Nothing past the class: AttributeError.
    class G:
        def m(self, x):
            return super().m(x)

    try:
        run(G(), 2)
    except AttributeError:
        pass
    else:
        raise AssertionError("expected AttributeError")


async def leaf(a):
    return a


async def nonleaf(a):
    if a < 0:
        raise ValueError(a)
    return a * 2


async def raiser(a):
    raise KeyError(a)


async def suspends(a):
    await asyncio.sleep(0)
    return a + 1


async def driver(n):
    t = 0
    for i in range(n):
        t += await leaf(i) + await nonleaf(i)
        try:
            await raiser(i)
        except KeyError as e:
            t += e.args[0]
    for i in range(50):
        t += await suspends(i)
    return t


def drive(c):
    try:
        while True:
            c.send(None)
    except StopIteration as e:
        return e.value


async def fib(n):
    if n <= 1:
        return n
    return await fib(n - 1) + await fib(n - 2)


def check_await():
    n = 5000
    want = sum(4 * i for i in range(n)) + sum(i + 1 for i in range(50))
    assert asyncio.run(driver(n)) == want
    assert drive(fib(18)) == 2584


check_super()
check_await()

print("super and await sites ok")
