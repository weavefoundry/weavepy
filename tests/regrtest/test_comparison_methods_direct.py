"""Comparison methods written in Python (`__eq__`, `__lt__` and the rest)
that compiled code calls for `==`, `<` and friends give what the
comparison protocol gives every time: the method's own answer (a `bool`
or anything else), the reflected method after `NotImplemented`, identity
or a `TypeError` when both decline, the answer's truth in a test, a raise
from inside the method, and operands released when the comparison ends."""

import dataclasses
import functools
import sys
import weakref

N = 3000
log = []


class Point:
    def __init__(self, x, y):
        self.x = x
        self.y = y

    def __eq__(self, other):
        if not isinstance(other, Point):
            return NotImplemented
        return self.x == other.x and self.y == other.y

    def __lt__(self, other):
        if not isinstance(other, Point):
            return NotImplemented
        return (self.x, self.y) < (other.x, other.y)

    def __le__(self, other):
        if not isinstance(other, Point):
            return NotImplemented
        return (self.x, self.y) <= (other.x, other.y)

    __hash__ = None


class Loose:
    """Answers anything, and says so."""

    def __init__(self, tag):
        self.tag = tag

    def __eq__(self, other):
        log.append(("eq", self.tag))
        return True

    def __gt__(self, other):
        log.append(("gt", self.tag))
        return "greater"

    def __ge__(self, other):
        log.append(("ge", self.tag))
        return []


class Shy:
    """Declines everything."""

    def __eq__(self, other):
        log.append("shy-eq")
        return NotImplemented

    def __lt__(self, other):
        log.append("shy-lt")
        return NotImplemented

    __hash__ = object.__hash__


def count_equal(a, b, n):
    hits = 0
    for _ in range(n):
        if a == b:
            hits += 1
    return hits


def count_less(a, b, n):
    hits = 0
    for _ in range(n):
        if a < b:
            hits += 1
    return hits


def values(a, b, n):
    out = None
    for _ in range(n):
        out = (a == b, a != b, a < b, a <= b)
    return out


for _ in range(3):
    p, q, r = Point(1, 2), Point(1, 2), Point(1, 3)
    assert count_equal(p, q, N) == N
    assert count_equal(p, r, N) == 0
    assert count_less(p, r, N) == N
    assert count_less(r, p, N) == 0
    assert values(p, q, N) == (True, False, False, True)
    assert values(p, r, N) == (False, True, True, True)

# `NotImplemented` against a plain value: the reflected side declines too,
# and equality falls back to identity.
p = Point(1, 2)
for other in (None, 7, "s", (1, 2), 2.5):
    assert count_equal(p, other, N) == 0
    assert count_equal(other, p, N) == 0
assert count_equal(p, p, N) == N


def ordering_error(a, b, n):
    hits = 0
    for _ in range(n):
        try:
            a < b
        except TypeError as e:
            assert "'<' not supported between instances of" in str(e), e
            hits += 1
    return hits


assert ordering_error(p, None, N) == N
assert ordering_error(p, 7, N) == N
assert ordering_error(Shy(), Shy(), 50) == 50

# `NotImplemented` against an instance with a method of its own: the
# reflected method runs, once, after the first.
log.clear()
shy, loose = Shy(), Loose("r")
assert count_equal(shy, loose, 200) == 200
assert log == ["shy-eq", ("eq", "r")] * 200, log[:6]
log.clear()
assert count_less(shy, loose, 200) == 200
assert log == ["shy-lt", ("gt", "r")] * 200, log[:6]
log.clear()
# Both decline: the method runs for each side, then identity answers.
other = Shy()
assert count_equal(shy, other, 100) == 0
assert log == ["shy-eq", "shy-eq"] * 100
log.clear()
assert count_equal(shy, shy, 100) == 100
assert log == ["shy-eq", "shy-eq"] * 100
log.clear()


# An answer that isn't a `bool` is the expression's value, and a test
# takes its truth.
def raw(a, b, n):
    out = None
    for _ in range(n):
        out = a > b
    return out


def truth_ge(a, b, n):
    hits = 0
    for _ in range(n):
        if a >= b:
            hits += 1
    return hits


loose = Loose("l")
assert raw(loose, 1, N) == "greater"
assert raw(loose, loose, N) == "greater"
assert truth_ge(loose, 1, N) == 0
assert truth_ge(loose, loose, N) == 0
log.clear()


class Verdict:
    def __init__(self, truth):
        self.truth = truth
        self.asked = 0

    def __bool__(self):
        self.asked += 1
        return self.truth


class Judge:
    def __init__(self, verdict):
        self.verdict = verdict

    def __eq__(self, other):
        return self.verdict


yes, no = Verdict(True), Verdict(False)
assert count_equal(Judge(yes), 1, N) == N and yes.asked == N
assert count_equal(Judge(no), Judge(no), N) == 0 and no.asked == N


def same_verdict(a, b, n):
    out = None
    for _ in range(n):
        out = a == b
    return out


assert same_verdict(Judge(yes), 1, N) is yes and yes.asked == N


class Moody:
    def __bool__(self):
        raise ValueError("no truth")


def moody(n):
    hits = 0
    j = Judge(Moody())
    for _ in range(n):
        try:
            if j == 1:
                pass
        except ValueError:
            hits += 1
    return hits


assert moody(N) == N


# A raise from inside the method passes through the comparison.
class Strict:
    def __init__(self, limit):
        self.limit = limit
        self.calls = 0

    def __eq__(self, other):
        self.calls += 1
        if self.calls > self.limit:
            raise KeyError(self.calls)
        return other is self

    def __lt__(self, other):
        return [k for k in range(3) if k > other][0]


def strict(n):
    s = Strict(n // 2)
    caught = 0
    for _ in range(n):
        try:
            assert s == s
        except KeyError:
            caught += 1
    return caught


assert strict(N) == N - N // 2


def index_error(n):
    s = Strict(0)
    caught = 0
    for i in range(n):
        try:
            assert (s < 0) == 1
            s < 5
        except IndexError:
            caught += 1
    return caught


assert index_error(N) == N

# Operands that only the comparison holds are released when it ends, the
# right one first, and before the next statement.
died = []


class Temp:
    def __init__(self, tag):
        self.tag = tag

    def __eq__(self, other):
        return isinstance(other, Temp) and len(died) >= 0

    def __lt__(self, other):
        return NotImplemented

    def __del__(self):
        died.append(self.tag)


def temps(n):
    hits = 0
    for i in range(n):
        if Temp(2 * i) == Temp(2 * i + 1):
            assert died[-2:] == [2 * i + 1, 2 * i], died[-4:]
            hits += 1
    return hits


assert temps(N) == N
assert len(died) == 2 * N
died.clear()


def temps_decline(n):
    hits = 0
    for i in range(n):
        try:
            Temp("a") < Temp("b")
        except TypeError:
            assert died[-2:] == ["b", "a"], died[-4:]
            hits += 1
    return hits


assert temps_decline(N) == N
died.clear()


def weak(n):
    for _ in range(n):
        t = Temp("w")
        ref = weakref.ref(t)
        assert t == Temp("x")
        del t
        assert ref() is None
    return True


assert weak(N)
died.clear()


# A subclass's reflected method goes first.
class Base:
    def __eq__(self, other):
        log.append("base")
        return True

    def __lt__(self, other):
        log.append("base-lt")
        return True


class Derived(Base):
    def __eq__(self, other):
        log.append("derived")
        return False

    def __gt__(self, other):
        log.append("derived-gt")
        return False


base, derived = Base(), Derived()
assert count_equal(base, derived, 100) == 0
assert log == ["derived"] * 100
log.clear()
assert count_less(base, derived, 100) == 0
assert log == ["derived-gt"] * 100
log.clear()
assert count_equal(derived, base, 100) == 0
assert log == ["derived"] * 100
log.clear()


# Methods that compare in turn, to any depth, and past the recursion
# limit.
class Node:
    def __init__(self, value, child=None):
        self.value = value
        self.child = child

    def __eq__(self, other):
        if not isinstance(other, Node):
            return NotImplemented
        return self.value == other.value and self.child == other.child


def chain(depth, last=0):
    node = Node(last)
    for k in range(depth):
        node = Node(k, node)
    return node


a, b, c = chain(30), chain(30), chain(30, last=1)
assert count_equal(a, b, 500) == 500
assert count_equal(a, c, 500) == 0
deep_a, deep_b = chain(sys.getrecursionlimit() + 50), chain(sys.getrecursionlimit() + 50)
for _ in range(3):
    try:
        deep_a == deep_b
    except RecursionError:
        pass
    else:
        raise AssertionError("no RecursionError")
assert count_equal(a, b, 500) == 500


# Generated methods: dataclasses and `functools.total_ordering`.
@dataclasses.dataclass(order=True)
class Version:
    major: int
    minor: int


@functools.total_ordering
class Rank:
    def __init__(self, n):
        self.n = n

    def __eq__(self, other):
        return isinstance(other, Rank) and self.n == other.n

    def __lt__(self, other):
        if not isinstance(other, Rank):
            return NotImplemented
        return self.n < other.n

    __hash__ = None


for _ in range(3):
    v1, v2, v3 = Version(1, 2), Version(1, 2), Version(1, 3)
    assert count_equal(v1, v2, N) == N
    assert count_equal(v1, v3, N) == 0
    assert count_equal(v1, (1, 2), N) == 0
    assert count_less(v1, v3, N) == N
    assert ordering_error(v1, (1, 2), 100) == 100
    r1, r2 = Rank(1), Rank(2)
    assert count_less(r1, r2, N) == N
    assert sum(r1 >= r2 for _ in range(N)) == 0
    assert sum(r2 >= r1 for _ in range(N)) == N
    assert sum(r1 != r2 for _ in range(N)) == N
    assert ordering_error(r1, 3, 100) == 100
    ranks = [Rank(k % 7) for k in range(200)]
    assert [r.n for r in sorted(ranks)] == sorted(k % 7 for k in range(200))
    assert max(ranks).n == 6 and min(ranks).n == 0


# Chained comparisons, comparisons as values, and comparisons in the
# other places a statement puts them.
def chained(lo, mid, hi, n):
    hits = 0
    for _ in range(n):
        if lo < mid <= hi:
            hits += 1
    return hits


assert chained(Point(0, 0), Point(1, 1), Point(1, 1), N) == N
assert chained(Point(0, 0), Point(2, 1), Point(1, 1), N) == 0
pts = [Point(k % 5, k % 3) for k in range(60)]
target = Point(4, 2)
for _ in range(50):
    assert sum(1 for p in pts if p == target) == 4
    assert [p == target for p in pts].count(True) == 4
    assert len([p for p in pts if not p < target]) == 4
    assert all((p == target) is (p.x == 4 and p.y == 2) for p in pts)


# A method swapped on the class, and one found through `__getattr__`-less
# plain lookup after the class changes.
class Swap:
    def __eq__(self, other):
        return True


s1, s2 = Swap(), Swap()
assert count_equal(s1, s2, N) == N
Swap.__eq__ = lambda self, other: False
assert count_equal(s1, s2, N) == 0
Swap.__eq__ = lambda self, other: NotImplemented
assert count_equal(s1, s2, N) == 0
assert count_equal(s1, s1, N) == N
del Swap.__eq__
assert count_equal(s1, s2, N) == 0
assert count_equal(s1, s1, N) == N


# A method that is a generator function, or takes extra parameters.
class Odd:
    def __eq__(self, other, extra=3):
        return extra == 3

    def __lt__(self, *args):
        return len(args) == 1

    def __le__(self, other):
        yield other


odd = Odd()
assert count_equal(odd, 1, N) == N
assert count_less(odd, 1, N) == N
assert sum(1 for _ in range(N) if odd <= 1) == N



# `x.__class__`, which these methods open with, in small functions that
# compiled code runs in line: every kind of receiver, a class that answers
# for itself, a re-classed instance, and a receiver that dies at the read.
def kind(x):
    return x.__class__


def same_kind(a, b):
    return a.__class__ is b.__class__


class Lying:
    @property
    def __class__(self):
        return int


class Hooked:
    def __getattribute__(self, name):
        if name == "__class__":
            return str
        return object.__getattribute__(self, name)


class Mortal:
    def __del__(self):
        died.append("mortal")


class Slotted:
    __slots__ = ("v",)


def kinds(n):
    p, q, lying, hooked, slotted = Point(0, 0), Point(1, 1), Lying(), Hooked(), Slotted()
    for i in range(n):
        assert kind(p) is Point and kind(1) is int and kind("s") is str
        assert kind(None) is type(None) and kind(Point) is type and kind(kind) is type(kinds)
        assert kind(lying) is int and kind(hooked) is str and kind(slotted) is Slotted
        assert same_kind(p, q) and not same_kind(p, 1) and same_kind(lying, 5)
        assert Mortal().__class__ is Mortal
        assert len(died) == i + 1
        assert kind(Mortal()) is Mortal
        assert len(died) == i + 2
        died.pop()
    return True


assert kinds(N)
died.clear()


class Before:
    pass


class After:
    pass


def reclass(n):
    obj = Before()
    seen = []
    for i in range(n):
        if i == n // 2:
            obj.__class__ = After
        seen.append(kind(obj))
    return seen


seen = reclass(N)
assert seen[: N // 2] == [Before] * (N // 2) and seen[N // 2 :] == [After] * (N - N // 2)

print("ok")
