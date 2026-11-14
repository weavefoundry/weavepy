"""`super().name` for names that aren't plain methods: values a base class
holds, class methods (from instance methods and from class methods), static
methods and properties, along the receiver's MRO and under changes to it,
at hot sites."""

N = 4000


class Base:
    limit = 10
    label = "base"
    flag = None
    ratio = 1.5

    @classmethod
    def create(cls, *args):
        return (cls.__name__, args)

    @staticmethod
    def helper(x):
        return x * 2

    @property
    def prop(self):
        return "base prop of " + type(self).__name__

    def method(self):
        return "base method"


class Mixin:
    limit = 20
    extra = "mixin"

    @classmethod
    def create(cls, *args):
        return ("mixin:" + cls.__name__, args)


class Child(Base):
    limit = 99
    label = "child"

    def __init__(self):
        self.limit = -1  # an instance attribute `super()` never sees

    def values(self):
        return super().limit, super().label, super().flag, super().ratio

    def make(self, *args):
        return super().create(*args)

    def bound(self):
        return super().create

    @classmethod
    def create(cls, *args):
        return ("child",) + super().create(*args)

    @classmethod
    def bound_cls(cls):
        return super().create

    def helped(self, x):
        return super().helper(x)

    @property
    def prop(self):
        return "child sees " + super().prop

    def method(self):
        return "child, then " + super().method()

    def missing(self):
        return super().nothing_here

    def dunder(self):
        return super().__class__, super().__doc__


class Grand(Child, Mixin):
    pass


c, g = Child(), Grand()
for _ in range(N):
    assert c.values() == (10, "base", None, 1.5)
    assert c.make(1, 2) == ("Child", (1, 2)) and g.make() == ("Grand", ())
    assert c.bound()() == ("Child", ()) and c.bound().__self__ is Child
    assert Child.create(3) == ("child", "Child", (3,))
    assert Grand.create() == ("child", "Grand", ())
    assert Child.bound_cls().__self__ is Child and Grand.bound_cls()() == ("Grand", ())
    assert c.helped(4) == 8 and c.prop == "child sees base prop of Child"
    assert g.prop == "child sees base prop of Grand"
    assert c.method() == "child, then base method"
    assert c.dunder() == (super, None)
for _ in range(100):
    try:
        c.missing()
    except AttributeError as e:
        assert "nothing_here" in str(e)
    else:
        raise AssertionError("expected AttributeError")


# The next class along the receiver's MRO, not the base's own base.
class Left(Base):
    limit = 1

    def see(self):
        return super().limit

    @classmethod
    def build(cls):
        return super().create()


class Right(Base):
    limit = 2

    @classmethod
    def create(cls, *args):
        return ("right:" + cls.__name__, args)


class Diamond(Left, Right):
    pass


l, d = Left(), Diamond()
for _ in range(N):
    assert l.see() == 10 and d.see() == 2
    assert Left.build() == ("Left", ()) and Diamond.build() == ("right:Diamond", ())

# Changes under a running loop: the base's value, and the bases themselves.
seen = []
for i in range(N):
    if i == 1000:
        Base.limit = 11
    if i == 2000:
        Base.limit = property(lambda self: "now a property")
    if i == 3000:
        del Base.limit
        Base.limit = 12.5
    seen.append(c.values()[0])
assert seen[999] == 10 and seen[1000] == 11 and seen[1999] == 11
assert seen[2000] == "now a property" and seen[3000] == 12.5 and seen[-1] == 12.5
Base.limit = 10


class Swap(Base):
    def see(self):
        return super().label, super().create()


s = Swap()
out = []
for i in range(N):
    if i == N // 2:
        Swap.__bases__ = (Mixin,)
    try:
        out.append(s.see())
    except AttributeError:
        out.append("no label")
assert out[0] == ("base", ("Swap", ())) and out[N // 2] == "no label" == out[-1]
print("ok")
