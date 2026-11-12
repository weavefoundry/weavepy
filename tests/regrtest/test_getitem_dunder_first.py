"""A subscript site that has run an instance's Python `__getitem__` calls it
first from compiled code, and still gives native containers', errors' and
changed classes' results: a list, dict or tuple at the same site, a
`__getitem__` that raises, `KeyError` from a dict at the site, a class whose
`__getitem__` is replaced or deleted, a subclass of a native container with
its own `__getitem__`, and an instance with `__class_getitem__` only."""


class Seq:
    def __init__(self, n):
        self.n = n

    def __getitem__(self, i):
        if i >= self.n:
            raise IndexError(i)
        return i * 2


class MyList(list):
    def __getitem__(self, i):
        return super().__getitem__(i) + 100


def total(c, n):
    t = 0
    for i in range(n):
        t += c[i]
    return t


s = Seq(1000)
for _ in range(5):
    assert total(s, 500) == sum(i * 2 for i in range(500))

# Other containers at the same site.
assert total(list(range(10)), 10) == 45
assert total(tuple(range(10)), 10) == 45
assert total({i: i for i in range(10)}, 10) == 45
assert total(MyList(range(10)), 10) == 45 + 1000
assert total(s, 10) == 90

# Errors raised by the method and by a native container.
try:
    total(Seq(5), 10)
except IndexError as e:
    assert e.args == (5,)
else:
    raise AssertionError("expected IndexError")
try:
    total({0: 1}, 3)
except KeyError as e:
    assert e.args == (1,)
else:
    raise AssertionError("expected KeyError")

# The class's method replaced, then deleted.
Seq.__getitem__ = lambda self, i: i + 1
assert total(s, 10) == 55
del Seq.__getitem__
try:
    total(s, 1)
except TypeError:
    pass
else:
    raise AssertionError("expected TypeError")


def getitem(self, i):
    return i * 2


Seq.__getitem__ = getitem
assert total(s, 10) == 90


# A stored value the method returns, kept alive across calls.
class Store:
    def __init__(self):
        self.items = [object() for _ in range(4)]

    def __getitem__(self, i):
        return self.items[i % 4]


def collect(c, n):
    out = []
    for i in range(n):
        out.append(c[i])
    return out


st = Store()
for _ in range(3):
    got = collect(st, 400)
    assert all(got[i] is st.items[i % 4] for i in range(400))
print("ok")
