"""Compiled code's reads of a built-in container's or string's method as a
value (`append = y.append`) keep CPython's results: the method bound to
the receiver read, equal to another read of it and not to another
receiver's, unbound for a class-level builtin, and a name that isn't a
method raising AttributeError."""


def grab(n):
    out = []
    for i in range(n):
        y = [i]
        append = y.append
        append(i + 1)
        d = {"k": i}
        get = d.get
        s = {i}
        add = s.add
        add(-1)
        up = "ab".upper
        out.append((y, get("k"), get("missing", 7), len(s), up()))
    return out


r = grab(3000)
assert r[-1] == ([2999, 3000], 2999, 7, 2, "AB"), r[-1]


def same(n):
    a, b = [], []
    for _ in range(n):
        assert a.append == a.append and a.append != b.append
        assert a.append.__self__ is a
        assert "x".join.__self__ == "x"
    return True


assert same(3000)


def missing(v, n):
    for _ in range(n):
        try:
            v.nope
        except AttributeError as e:
            last = str(e)
    return last


assert "nope" in missing([], 3000) and "nope" in missing("s", 10)
print("ok")
