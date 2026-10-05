"""Leaf functions that call read-only builtins keep their results, errors,
and callbacks."""
import math


class Box:
    def __init__(self, v):
        self.v = v


class Key:
    calls = 0

    def __init__(self, k):
        self.k = k

    def __hash__(self):
        return hash(self.k)

    def __eq__(self, other):
        Key.calls += 1
        return isinstance(other, Key) and other.k == self.k


class Sized:
    def __len__(self):
        return 7


def f_len(x, y):
    return len(x) + y


def f_isinstance(x):
    return isinstance(x, int)


def f_get(d, k):
    return d.get(k, -1)


def f_sqrt(x):
    return math.sqrt(x) + 1.0


def f_upper(s):
    return s.upper()


def f_join(s, parts):
    return s.join(parts)


def f_format(s, a, b):
    return s.format(a, b)


def f_append(items, x):
    return items.append(x)


def f_mixed(box, seq):
    return len(seq) + box.v


def f_nested(x, seq):
    return f_len(seq, x) * 2


def f_floor(x):
    return math.floor(x)


for i in range(3000):
    assert f_len([1, 2, 3], i) == 3 + i
    assert f_len("abcd", i) == 4 + i
    assert f_len({1: 2}, i) == 1 + i
    assert f_len(Sized(), i) == 7 + i
    assert f_isinstance(i) is True
    assert f_isinstance("x") is False
    assert f_isinstance(True) is True
    assert f_get({"a": i}, "a") == i
    assert f_get({"a": i}, "b") == -1
    assert f_get({1: i}, 1.0) == i
    assert f_sqrt(float(i)) == math.sqrt(i) + 1.0
    assert f_upper("ab%d" % i) == "AB%d" % i
    assert f_join("-", ["a", str(i)]) == "a-%d" % i
    assert f_format("{}:{}", i, 2.5) == "%d:2.5" % i
    items = []
    assert f_append(items, i) is None and items == [i]
    assert f_mixed(Box(i), (1, 2)) == 2 + i
    assert f_nested(i, [0] * (i % 5)) == 2 * (i % 5 + i)
    assert f_floor(i / 3) == i // 3
    if i % 500 == 0:
        try:
            f_sqrt(-1.0)
        except ValueError as e:
            assert str(e) == "expected a nonnegative input, got -1.0", e
        else:
            raise AssertionError("sqrt(-1.0) returned")
        try:
            f_len(5, 1)
        except TypeError as e:
            assert "has no len()" in str(e), e
        else:
            raise AssertionError("len(5) returned")
        try:
            f_floor(float("inf"))
        except OverflowError:
            pass
        else:
            raise AssertionError("floor(inf) returned")
        try:
            f_upper(5)
        except AttributeError:
            pass
        else:
            raise AssertionError("(5).upper() returned")
        # A key whose comparison runs Python code takes the ordinary call.
        before = Key.calls
        assert f_get({Key(1): 2}, Key(1)) == 2
        assert Key.calls == before + 1
print("ok")


# A native leaf that returns a value it owns (a builtin call's fresh
# result) hands back its own reference, not a pointer into its scratch.
def f_fresh(s, a):
    return s.format(a)


for i in range(3000):
    assert f_fresh("{}!", i) == "%d!" % i
print("fresh ok")
