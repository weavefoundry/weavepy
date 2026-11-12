"""Keyword calls of native callees from compiled loops give what the
general call gives: builtin functions, a builtin type's methods (through
the method form, a bound method held in a local, and the type's own
function with the receiver first), builtin types, callbacks that run
Python code or raise, wrong and repeated keywords, receivers that are
subclass instances, and call sites whose keyword names or callee change."""

import collections
import functools


def sort_method(n):
    out = None
    for i in range(n):
        xs = [3, 1, 2, i % 5]
        xs.sort(reverse=True)
        ys = [-3, 1, -2]
        ys.sort(key=abs, reverse=i % 2 == 0)
        out = (xs, ys)
    return out


def str_methods(n):
    acc = 0
    for i in range(n):
        s = "a b c d"
        acc += len(s.split(maxsplit=1)) + len(s.split(sep=" ", maxsplit=i % 3))
        acc += len("x,y".rsplit(sep=",")) + len(b"1 2".split(maxsplit=1))
        acc += "{a}-{b}".format(a=i, b=2).count("-")
        acc += len("abc".encode(encoding="utf-8", errors="strict"))
    return acc


def functions(n):
    acc = 0
    for i in range(n):
        xs = [3, -1, 2]
        acc += sorted(xs, reverse=True)[0]  # 3
        acc += sorted(xs, key=abs)[0]  # -1
        acc += min(xs, key=abs)  # -1
        acc += max(xs, key=lambda v: -v)  # -1
        acc += max([], default=7)
        acc += sum(xs, start=i)  # 4 + i
        acc += int(round(1.26, ndigits=1) * 10)  # 13
        acc += int("101", base=2)  # 5
        acc += len(list(enumerate(xs, start=i)))  # 3
        acc += len(list(zip(xs, xs, strict=True)))  # 3
        acc += len(dict(a=i, b=2))  # 2
        acc += len(str(b"ab", encoding="ascii"))  # 2
        acc += len(bytes("ab", encoding="utf-8"))  # 2
    return acc


def bound_in_local(n):
    xs = []
    sort = xs.sort
    get = {"a": 1}.get
    acc = 0
    for i in range(n):
        xs[:] = [2, 3, 1]
        sort(reverse=True)
        acc += xs[0]
    return acc + get("a")


def through_the_type(n):
    acc = 0
    for i in range(n):
        xs = [2, 3, 1]
        list.sort(xs, reverse=True)
        acc += xs[0] + len(str.split("a b", maxsplit=1))
    return acc


class Keyed:
    calls = 0

    def __init__(self, v):
        self.v = v

    def key(self):
        Keyed.calls += 1
        return -self.v


def python_callbacks(n):
    acc = 0
    for i in range(n):
        items = [Keyed(1), Keyed(3), Keyed(2)]
        items.sort(key=Keyed.key)
        acc += items[0].v + sorted(items, key=lambda k: k.v)[0].v
        acc += min(items, key=Keyed.key).v
    return acc


def raising_callbacks(n):
    hits = 0
    for i in range(n):
        try:
            [1, 2].sort(key=lambda v: 1 // (v - 2))
        except ZeroDivisionError:
            hits += 1
        try:
            sorted([1, "a"], reverse=True)
        except TypeError:
            hits += 1
    return hits


def bad_keywords(n):
    hits = 0
    for i in range(n):
        try:
            [1].sort(revers=True)
        except TypeError as e:
            assert "revers" in str(e), e
            hits += 1
        try:
            "a".split(maxsplt=1)
        except TypeError as e:
            assert "maxsplt" in str(e), e
            hits += 1
        try:
            len([], x=1)
        except TypeError as e:
            assert "keyword" in str(e), e
            hits += 1
        try:
            int("1", bas=2)
        except TypeError as e:
            assert "bas" in str(e), e
            hits += 1
    return hits


class MyList(list):
    def sort(self, *, reverse=False):
        super().sort(reverse=not reverse)
        return "mine"


class Plain(list):
    pass


def subclass_receivers(n):
    out = None
    for i in range(n):
        a = MyList([1, 3, 2])
        r = a.sort(reverse=True)
        b = Plain([1, 3, 2])
        b.sort(reverse=True)
        d = collections.OrderedDict(a=1, b=2)
        d.move_to_end("a", last=True)
        out = (r, list(a), list(b), list(d), d.popitem(last=False))
    return out


def changing_sites(n):
    acc = []
    for i in range(n):
        xs = [2, 3, 1]
        f = sorted if i % 2 else functools.partial(sorted, key=lambda v: v)
        if i % 3:
            acc.append(f(xs, reverse=True)[0])
        else:
            acc.append(f(xs, reverse=False)[0])
        obj = xs if i % 2 else collections.deque(xs)
        if isinstance(obj, list):
            obj.sort(reverse=True)
        else:
            obj.rotate(1)
        acc.append(obj[0])
    return sum(acc)


def nested(n):
    acc = 0
    for i in range(n):
        acc += sorted([3, 1, 2], key=lambda v: min([v, 9], key=lambda w: -w))[0]
        acc += max([[2, 1], [3]], key=lambda xs: sorted(xs, reverse=True)[0])[0]
    return acc


def printing(n):
    import io

    buf = io.StringIO()
    for i in range(n):
        print(i, i, sep="-", end=";", file=buf)
    return buf.getvalue()


N = 4000
for _ in range(3):
    assert sort_method(N) == (sorted([3, 1, 2, (N - 1) % 5], reverse=True), [1, -2, -3])
    assert str_methods(N) == sum(2 + len("a b c d".split(" ", i % 3)) + 2 + 2 + 1 + 3 for i in range(N))
    assert functions(N) == sum(41 + i for i in range(N))
    assert bound_in_local(N) == 3 * N + 1
    assert through_the_type(N) == 5 * N
    Keyed.calls = 0
    assert python_callbacks(N) == (3 + 1 + 3) * N
    assert Keyed.calls == 6 * N
    assert raising_callbacks(N) == 2 * N
    assert bad_keywords(N) == 4 * N
    assert subclass_receivers(N) == ("mine", [1, 2, 3], [3, 2, 1], ["b", "a"], ("b", 2))
    assert changing_sites(N) == sum((3 if i % 3 else 1) + (3 if i % 2 else 1) for i in range(N))
    assert nested(N) == (3 + 3) * N
    assert printing(50) == "".join("%d-%d;" % (i, i) for i in range(50))
