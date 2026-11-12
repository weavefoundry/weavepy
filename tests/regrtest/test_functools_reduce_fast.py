"""`functools.reduce` over tuples, lists and other iterables, with native
and Python functions, gives CPython's results and errors: an initial value
(by position and keyword), an empty iterable, operands a native half
declines (strings, big ints, instances with `__add__`), a list the function
grows, and errors from the function."""

import functools
import operator

reduce = functools.reduce

assert reduce(operator.add, (1, 2, 3)) == 6
assert reduce(operator.add, [1, 2, 3]) == 6
assert reduce(operator.add, iter([1, 2, 3])) == 6
assert reduce(operator.add, (1, 2, 3), 10) == 16
assert reduce(operator.add, [1, 2, 3], initial=10) == 16
assert reduce(operator.add, (), 5) == 5
assert reduce(operator.add, [], 5) == 5
assert reduce(operator.add, (7,)) == 7
assert reduce(operator.mul, range(1, 6)) == 120
assert reduce(operator.add, ("a", "b", "c")) == "abc"
assert reduce(operator.add, [10 ** 30, 10 ** 30]) == 2 * 10 ** 30
assert reduce(operator.add, (1.5, 2)) == 3.5
assert reduce(lambda a, b: a * 10 + b, [1, 2, 3]) == 123


class V:
    def __init__(self, v):
        self.v = v

    def __add__(self, other):
        return V(self.v + other.v)


assert reduce(operator.add, [V(1), V(2), V(3)]).v == 6

for empty in ((), []):
    try:
        reduce(operator.add, empty)
    except TypeError as e:
        assert str(e) == "reduce() of empty iterable with no initial value", e
    else:
        raise AssertionError("expected TypeError")

try:
    reduce(operator.add, (1, "a"))
except TypeError:
    pass
else:
    raise AssertionError("expected TypeError")


# A function that grows the list it reduces: the iteration sees the
# growth, as a list iterator does.
lst = [1, 2, 3]


def grow(a, b):
    if len(lst) < 6:
        lst.append(b * 10)
    return a + b


assert reduce(grow, lst) == 1 + 2 + 3 + 20 + 30 + 200, lst
assert lst == [1, 2, 3, 20, 30, 200]


def boom(a, b):
    raise ValueError("boom")


try:
    reduce(boom, (1, 2))
except ValueError as e:
    assert str(e) == "boom"
else:
    raise AssertionError("expected ValueError")

# In a hot loop.
tup = tuple(range(10))
for _ in range(2000):
    assert reduce(operator.add, tup) == 45
print("ok")
