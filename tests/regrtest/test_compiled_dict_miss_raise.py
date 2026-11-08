"""A compiled `d[key]` that misses raises CPython's `KeyError`: the key as
its only argument (a string, an int, a tuple), caught by `except KeyError`
and by `except LookupError`, uncaught through a caller, with a key held
only by the subscript, and through a `dict` subclass's `__missing__`,
which still runs."""


class Counting(dict):
    def __missing__(self, key):
        return ("missing", key)


def catch_all(d, keys):
    out = []
    for k in keys:
        try:
            out.append(d[k])
        except KeyError as e:
            out.append(("KeyError", e.args, type(e.args[0]).__name__))
    return out


def catch_lookup(d, keys):
    out = []
    for k in keys:
        try:
            out.append(d[k])
        except LookupError as e:
            out.append(type(e).__name__)
    return out


def inner(d, k):
    return d[k]


def outer(d, k):
    try:
        return inner(d, k)
    except KeyError as e:
        return "outer caught %r" % (e.args,)


def fresh_key(d, n):
    out = []
    for i in range(n):
        try:
            out.append(d["k%d" % i])
        except KeyError as e:
            out.append(e.args[0])
    return out


d = {"a": 1, 2: "two", (1, 2): "pair"}
keys = ["a", "b", 2, 3, (1, 2), (2, 1)]
for _ in range(300):
    got = catch_all(d, keys)
assert got == [
    1,
    ("KeyError", ("b",), "str"),
    "two",
    ("KeyError", (3,), "int"),
    "pair",
    ("KeyError", ((2, 1),), "tuple"),
], got

for _ in range(300):
    got = catch_lookup(d, keys)
assert got == [1, "KeyError", "two", "KeyError", "pair", "KeyError"], got

for _ in range(300):
    got = [outer(d, k) for k in ("a", "zz", 7)]
assert got == [1, "outer caught ('zz',)", "outer caught (7,)"], got

for _ in range(300):
    got = fresh_key({"k1": 10}, 3)
assert got == ["k0", 10, "k2"], got

c = Counting(a=1)
for _ in range(300):
    got = catch_all(c, ["a", "x"])
assert got == [1, ("missing", "x")], got

try:
    for _ in range(300):
        inner(d, "nope")
except KeyError as e:
    assert e.args == ("nope",), e.args
else:
    raise AssertionError("no KeyError")

print("ok")
