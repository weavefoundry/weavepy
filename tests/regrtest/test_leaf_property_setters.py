"""A property whose Python setter is one store (`self._w = value`), which
runs without a frame, keeps CPython's results: the stored value, a
slotted class's member, a constant stored, a setter replaced or
deleted later, a subclass's override, a setter that does more, and the
references compiled stores give back (the value's, a receiver's from the
stack, and a replaced value's finalizer)."""
import sys


class Box:
    def __init__(self, w):
        self._w = w

    @property
    def width(self):
        return self._w

    @width.setter
    def width(self, v):
        self._w = v

    @property
    def flag(self):
        return self._f

    @flag.setter
    def flag(self, v):
        self._f = True


class Slotted:
    __slots__ = ("_w",)

    def __init__(self):
        self._w = 0

    @property
    def width(self):
        return self._w

    @width.setter
    def width(self, v):
        self._w = v


class Wide(Box):
    @Box.width.setter
    def width(self, v):
        self._w = v * 2


class Counted(Box):
    sets = 0

    @Box.width.setter
    def width(self, v):
        Counted.sets += 1
        self._w = v


def run(objs, n):
    for i in range(n):
        for o in objs:
            o.width = o.width + 1
    return [o.width for o in objs]


objs = [Box(1), Slotted(), Wide(1), Counted(1)]
# Past the frame JIT's threshold, so later rounds run compiled code.
for k in range(1, 301):
    assert run(objs, 10)[:2] == [1 + 10 * k, 10 * k]
assert Counted.sets == 3000 and objs[3].width == 3001
b = Box(0)
b.flag = 5
assert b.flag is True and b.__dict__ == {"_w": 0, "_f": True}


class Dies:
    died = 0

    def __del__(self):
        Dies.died += 1


def refs(holders, v, n):
    for i in range(n):
        holders[0].width = v  # the receiver from the stack
        h = holders[1]
        h.width = v  # the receiver a local
        h.width = Dies()  # the last value's owner, replaced next round


holders = [Box(0), Box(0)]
token = object()
base = sys.getrefcount(token)
for k in range(300):
    refs(holders, token, 10)
assert sys.getrefcount(token) == base + 1, (sys.getrefcount(token), base)
assert holders[0].width is token and Dies.died == 2999, Dies.died

# The setter replaced: later stores run the new one.
Box.width = Box.width.setter(lambda self, v: setattr(self, "_w", -v))
objs[0].width = 4
assert objs[0].width == -4
# A read-only property: the store raises.
Box.width = property(lambda self: self._w)
try:
    objs[0].width = 1
except AttributeError:
    pass
else:
    raise AssertionError("a property without a setter took a store")
print("ok")
