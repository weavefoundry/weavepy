"""A bound method called with a spread tuple or list (`f(*args)` where
`f = obj.method`), whose function compiled code binds straight from the
spread, keeps CPython's results: the receiver first, defaults and
`*args`, an arity error's message, a raise with its traceback, a list
spread, recursion, and a method rebound between calls."""
import traceback


class Box:
    def __init__(self, k):
        self.k = k

    def add(self, a, b=10):
        t = 0
        for v in (a, b):
            t += v
        return t + self.k

    def star(self, *rest):
        out = [self.k]
        for v in rest:
            out.append(v)
        return out

    def boom(self, x):
        if x < 0:
            raise ValueError(x)
        return [x, self.k]

    def depth(self, n):
        f = self.depth
        return 0 if n == 0 else 1 + f(*(n - 1,))


def run(i):
    b = Box(i)
    add, star, boom, depth = b.add, b.star, b.boom, b.depth
    assert add(*(1, 2)) == 3 + i and add(*(5,)) == 15 + i and add(*[1, 2]) == 3 + i
    assert star(*()) == [i] and star(*(1, 2, 3)) == [i, 1, 2, 3]
    assert boom(*(2,)) == [2, i]
    try:
        add(*(1, 2, 3))
    except TypeError as e:
        assert str(e) == "Box.add() takes from 2 to 3 positional arguments but 4 were given", e
    else:
        raise AssertionError("an arity error was lost")
    try:
        boom(*(-1,))
    except ValueError as e:
        assert [f.name for f in traceback.extract_tb(e.__traceback__)][-1] == "boom"
    else:
        raise AssertionError("the method's raise was lost")
    assert depth(*(20,)) == 20
    m = Box(1).add if i % 2 else b.star
    assert m(*(1, 1)) == (3 if i % 2 else [i, 1, 1])


# Past the frame JIT's threshold, so later rounds run compiled code.
for i in range(2000):
    run(i)
print("ok")
