"""Builtin functions and container algebra on their fast paths behave as
CPython's: frame-sensitive builtins still see their caller, errors keep
their tracebacks, set operations pick the same elements, and dict.update
keeps its observable effects."""

import gc
import sys
import warnings
import weakref


def hot(fn, n=300):
    out = None
    for _ in range(n):
        out = fn()
    return out


# Frame-sensitive builtins, called often enough for their sites to settle.
def who_called():
    return sys._getframe(1).f_code.co_name


def outer_caller():
    return who_called()


assert hot(outer_caller) == "outer_caller"


def local_names(a, b=2):
    c = a + b
    return sorted(locals())


assert hot(lambda: local_names(1)) == ["a", "b", "c"]


def warn_here():
    warnings.warn("careful", UserWarning, stacklevel=2)


def warning_site():
    warn_here()  # the warning is attributed to this line


with warnings.catch_warnings(record=True) as caught:
    warnings.simplefilter("always")
    for _ in range(50):
        warning_site()
assert caught and all(w.lineno == warning_site.__code__.co_firstlineno + 1 for w in caught)

# The builtin-function lane: results, errors, and Python callbacks.
assert hot(lambda: min(3, 1, 2)) == 1
assert hot(lambda: max([4, 9, 2], key=lambda v: -v)) == 2
assert hot(lambda: abs(-7)) == 7
assert hot(lambda: sorted([3, 1, 2], reverse=True)) == [3, 2, 1]
assert hot(lambda: getattr(sys, "nonexistent", 5)) == 5


def bad_min():
    return min([])


for _ in range(5):
    try:
        bad_min()
    except ValueError as e:
        assert e.__traceback__.tb_next.tb_frame.f_code.co_name == "bad_min"
    else:
        raise AssertionError("min of an empty list")


class Loud:
    def __lt__(self, other):
        raise KeyError("compare")


try:
    hot(lambda: min(Loud(), Loud()), 3)
except KeyError:
    pass
else:
    raise AssertionError("min swallowed a comparison error")

# Set algebra: CPython's iteration order picks the surviving element.
a, b = {1, 2.0, 3}, {1.0, 2, 4}
r = hot(lambda: a & b)
assert sorted(r) == [1, 2] and {type(x) for x in r} == {float, int}
assert [type(x) for x in {1} & {1.0, 5}] == [int] and [type(x) for x in {1.0} & {1}] == [int]
assert type(frozenset(a) & b) is frozenset and type(a & frozenset(b)) is set
assert sorted(a | b) == [1, 2, 3, 4] and sorted(a - b) == [3] and sorted(a ^ b) == [3, 4]


class Collide:
    def __init__(self, n):
        self.n = n

    def __hash__(self):
        return 7

    def __eq__(self, other):
        return isinstance(other, Collide) and other.n == self.n


s = {Collide(1), Collide(2)}
assert len(s & {Collide(2), Collide(3)}) == 1

# dict.update: overwrites keep the original key and position, new keys
# append, and colliding custom keys compare through __eq__.
d = {"x": 0, 1: 1}
hot(lambda: d.update({"x": 5, 3: 3}), 1)
assert list(d.items()) == [("x", 5), (1, 1), (3, 3)]
e = {Collide(1): "a"}
e.update({Collide(1): "b", Collide(2): "c"})
assert [k.n for k in e] == [1, 2] and list(e.values()) == ["b", "c"]


# An instance __dict__ filled by update is still traced: a cycle through it
# is collected.
class Holder:
    pass


h = Holder()
h.__dict__.update({"me": h, "n": 1})
ref = weakref.ref(h)
del h
gc.collect()
assert ref() is None, "cycle through an updated __dict__ survived"
print("ok")
