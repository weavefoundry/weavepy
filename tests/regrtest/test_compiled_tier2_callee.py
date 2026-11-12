"""A small function whose loop tier 2 compiled, called from a loop that the
frame JIT compiles (and may take the callee in line), gives what the
interpreter gives: when the global it iterates is rebound, when its
elements change type, when it raises, when the callee's code is replaced,
and through a method."""

import sys

t = tuple(range(10))


def doubled():
    return [x * 2 for x in t]


def total():
    s = 0
    for x in t:
        s += x
    return s


def call_doubled(n):
    out = None
    for _ in range(n):
        out = doubled()
    return out


def call_total(n):
    acc = 0
    for _ in range(n):
        acc += total()
    return acc


for _ in range(3):
    assert call_doubled(3000) == [x * 2 for x in range(10)]
    assert call_total(3000) == 3000 * 45

# The iterated global rebound, then holding other types.
t = (1.5, 2.5)
assert call_doubled(500) == [3.0, 5.0]
assert call_total(500) == 500 * 4.0
t = ("a", "b")
assert call_doubled(500) == ["aa", "bb"]
t = [3, 4, 5]
assert call_doubled(500) == [6, 8, 10]
assert call_total(500) == 500 * 12
t = tuple(range(10))


# An error inside the callee propagates through the caller, with both
# frames in the traceback.
def bad():
    return [1 // x for x in t]


def call_bad(n):
    for _ in range(n):
        bad()


t = (1, 2, 3)
call_bad(3000)
t = (1, 0)
try:
    call_bad(10)
except ZeroDivisionError:
    names = []
    tb = sys.exc_info()[2]
    while tb is not None:
        names.append(tb.tb_frame.f_code.co_name)
        tb = tb.tb_next
    assert names[-2:] == ["call_bad", "bad"], names
else:
    raise AssertionError("no ZeroDivisionError")
t = tuple(range(10))

# The callee's code replaced after the caller compiled.
doubled.__code__ = (lambda: [x * 3 for x in t]).__code__
assert call_doubled(500) == [x * 3 for x in range(10)]


# A method whose loop tier 2 compiled.
class Acc:
    def __init__(self):
        self.items = list(range(8))

    def sum_sq(self):
        s = 0
        for v in self.items:
            s += v * v
        return s


def call_method(a, n):
    r = 0
    for _ in range(n):
        r = a.sum_sq()
    return r


a = Acc()
assert call_method(a, 3000) == sum(v * v for v in range(8))
a.items.append(10)
assert call_method(a, 100) == sum(v * v for v in range(8)) + 100


# The callee sees its caller's frame.
def depth_and_loop():
    s = 0
    for x in t:
        s += x
    return sys._getframe(1).f_code.co_name, s


def call_depth(n):
    r = None
    for _ in range(n):
        r = depth_and_loop()
    return r


assert call_depth(3000) == ("call_depth", 45)


# A function made anew at each call of its caller (the site sees a new
# function object every time).
def nested(n):
    def f():
        return [x * 2 for x in t]

    out = None
    for _ in range(n):
        out = f()
    return out


for _ in range(3):
    assert nested(3000) == [x * 2 for x in range(10)]


# A compiled loop over a list its body grows, and over one whose elements
# change type part way.
def grow(xs):
    total = 0
    for x in xs:
        if x < 5:
            xs.append(x + 10)
        total += x
    return total


def grow_many(n):
    r = 0
    for _ in range(n):
        r = grow([0, 1, 2, 3, 4, 5])
    return r


assert grow_many(2000) == sum(range(6)) + sum(range(10, 15))


def mixed_sum(xs):
    s = 0
    for x in xs:
        s += x
    return s


for _ in range(2000):
    assert mixed_sum([1, 2, 3]) == 6
assert mixed_sum([1, 2.5, 3]) == 6.5
assert mixed_sum((1, True, 10**30)) == 2 + 10**30
print("ok")
