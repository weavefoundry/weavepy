"""`vars(obj)` and `pow(...)` at a hot site give what the attribute
protocol and the `**` operator give, for the shapes they serve in place and
for the ones they leave to the general call."""

import math

N = 3000


class Plain:
    def __init__(self):
        self.a = 1


class WithProp:
    @property
    def __dict__(self):
        return {"fake": True}


class Slotted:
    __slots__ = ("a",)


class Hooked:
    def __getattribute__(self, name):
        if name == "__dict__":
            return {"hooked": 1}
        return object.__getattribute__(self, name)


class SubDict(dict):
    pass


p, w, h, sd = Plain(), WithProp(), Hooked(), SubDict()
sd.x = 5
for i in range(N):
    v = vars(p)
    assert v is p.__dict__ and v == {"a": 1}
    assert vars(w) == {"fake": True} and vars(h) == {"hooked": 1} and vars(sd) == {"x": 5}
    assert vars(Plain)["__init__"] is Plain.__init__ and vars(math)["pi"] == math.pi
vars(p)["b"] = 2
assert p.b == 2
for bad in (Slotted(), 3, "s", [1]):
    for _ in range(50):
        try:
            vars(bad)
        except TypeError:
            pass
        else:
            raise AssertionError("vars() of %r" % (bad,))

ints = [-7, -2, -1, 0, 1, 2, 3, 10, 97, 2**31, 2**62]
exps = [0, 1, 2, 3, 5, 31, 62, 63, 64, 100]
mods = [1, 2, 7, -7, 97, 2**31 - 1, -(2**40), 2**62 + 1]
for _ in range(3):
    for b in ints:
        for e in exps:
            assert pow(b, e) == b**e and type(pow(b, e)) is int
            for m in mods:
                assert pow(b, e, m) == (b**e) % m, (b, e, m)
        for e in (-1, -2, -5):
            if b:
                assert pow(b, e) == b**e and type(pow(b, e)) is float
for _ in range(N):
    assert pow(2, 10) == 1024 and pow(2, 10, 1000) == 24 and pow(2, -1) == 0.5
    assert pow(2.0, 3) == 8.0 and pow(2, 0.5) == 2**0.5 == math.sqrt(2)
    assert pow(9.0, 0.5) == 3.0 and pow(2, 10, mod=1000) == 24 and pow(True, 3) == 1
    assert pow(-8, 1 / 3) == (-8) ** (1 / 3) and isinstance(pow(-8, 1 / 3), complex)
    assert pow(3, 4, -5) == -4 and pow(-3, 3, 5) == 3 and pow(5, 0, 1) == 0
    assert pow(3, -1, 7) == 5  # a modular inverse
for args, exc in (
    ((0, -1), ZeroDivisionError),
    ((2, 3, 0), ValueError),
    ((10.0, 400), OverflowError),
    ((2.0, 3, 5), TypeError),
    ((0.0, -1.0), ZeroDivisionError),
    (("a", 2), TypeError),
):
    for _ in range(50):
        try:
            pow(*args)
        except exc:
            pass
        else:
            raise AssertionError("pow%r should raise %s" % (args, exc.__name__))
assert pow(2**70, 2) == 2**140 and pow(3, 200) == 3**200 and pow(2**70, 3, 1000) == (2**210) % 1000
print("ok")
