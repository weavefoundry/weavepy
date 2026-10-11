"""`obj[key] = value` on an instance whose class defines `__setitem__`
in Python: the method runs with the right arguments at every execution
of a warm site, its result is discarded, exceptions carry its frame, a
changed `__setitem__` takes effect, and replaced values die on time."""

import gc


class Store:
    def __init__(self):
        self.log = []

    def __setitem__(self, key, value):
        self.log.append((key, value))
        return "ignored"


s = Store()
for i in range(300):
    s[i] = i * 2
    s["k"] = (s, i)
assert s.log[:2] == [(0, 0), ("k", (s, 0))]
assert len(s.log) == 600


# A dict subclass funnelling stores through its own logic.
class Doubling(dict):
    def __setitem__(self, key, value):
        dict.__setitem__(self, key, value * 2)


d = Doubling()
for i in range(300):
    d[i % 7] = i
assert d[299 % 7] == 598 and len(d) == 7


# Exceptions propagate with the method's frame in the traceback.
class Picky:
    def __setitem__(self, key, value):
        if key < 0:
            raise KeyError(key)


p = Picky()
for i in range(200):
    p[i] = i
    try:
        p[-i - 1] = i
    except KeyError as e:
        assert e.args == (-i - 1,)
        tb = e.__traceback__
        names = []
        while tb is not None:
            names.append(tb.tb_frame.f_code.co_name)
            tb = tb.tb_next
        assert names[-1] == "__setitem__", names
    else:
        raise AssertionError("expected KeyError")


# Reassigning `__setitem__` after warm-up, including to one that takes
# defaults or isn't a plain function.
class Swappable:
    def __setitem__(self, key, value):
        self.last = ("first", key, value)


sw = Swappable()


def store(obj, k, v):
    obj[k] = v


for i in range(300):
    store(sw, i, i)
assert sw.last == ("first", 299, 299)


def second(self, key, value, extra="x"):
    self.last = ("second", key, value, extra)


Swappable.__setitem__ = second
for i in range(50):
    store(sw, i, i)
assert sw.last == ("second", 49, 49, "x")


seen = []
Swappable.__setitem__ = staticmethod(lambda k, v: seen.append(("static", k, v)))
store(sw, 1, 2)
assert seen == [("static", 1, 2)]
del Swappable.__setitem__
try:
    store(sw, 1, 2)
except TypeError as e:
    assert "does not support item assignment" in str(e), str(e)
else:
    raise AssertionError("expected TypeError")


# A replaced value dies when the store drops it, and the stored value
# and key are released when the method is done with them.
class Tracked:
    alive = 0

    def __init__(self):
        Tracked.alive += 1

    def __del__(self):
        Tracked.alive -= 1


class Drop:
    def __setitem__(self, key, value):
        pass


dr = Drop()


def drop_store():
    dr[Tracked()] = Tracked()
    return Tracked.alive


gc.disable()
try:
    for _ in range(300):
        assert drop_store() == 0
finally:
    gc.enable()

print("ok")
