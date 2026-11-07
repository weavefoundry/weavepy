"""Attribute reads compiled code serves from its sites' caches keep
CPython's results: a base-class method reading fields of several
subclasses' instances (in each one's own attribute order), a class
constant read through the class (`Cls.CONST`, from inlined bodies and
plain ones alike), and every change since: a constant rebound, a field
deleted or shadowed by a property, an instance that gained a `__dict__` of
its own, or one whose fields went in a different order."""


class Base:
    NONE = 0
    FORWARD = 1

    def __init__(self, a, b):
        self.a = a
        self.b = b

    def get_a(self):
        return self.a

    def get_b(self):
        return self.b

    def forward(self):
        return self.b == Base.FORWARD


class Left(Base):
    pass


class Right(Base):
    def __init__(self, a, b):
        # The other order: `b` takes the first position.
        self.b = b
        self.a = a


class Third(Base):
    def __init__(self, a, b):
        self.extra = None
        super().__init__(a, b)


def total(objs):
    t = 0
    for o in objs:
        t += o.get_a() * 10 + o.get_b() + o.forward()
    return t


def plain_constants(n):
    t = 0
    for _ in range(n):
        t += Base.FORWARD + Left.NONE
    return t


objs = [Left(1, 1), Right(2, 0), Third(3, 1), Base(4, 1)]
expect = sum(o.a * 10 + o.b + (o.b == 1) for o in objs)
# Past the frame JIT's threshold, so later rounds run compiled code.
for _ in range(3000):
    assert total(objs) == expect
    assert plain_constants(3) == 3

# A rebound constant is seen at once, through the class and its subclass.
Base.FORWARD = 0
assert total(objs) == sum(o.a * 10 + o.b + (o.b == 0) for o in objs)
assert plain_constants(2) == 0
Left.NONE = 5
assert plain_constants(2) == 10
Base.FORWARD = 1
del Left.NONE
assert plain_constants(2) == 2

# A deleted field raises; a field put back reads again.
r = objs[1]
del r.a
try:
    total(objs)
except AttributeError:
    pass
else:
    raise AssertionError("a deleted field was read")
r.a = 7
assert total(objs) == sum(o.a * 10 + o.b + (o.b == 1) for o in objs)

# An instance whose __dict__ was taken out (vars()) and changed through it.
vars(objs[0])["a"] = 9
assert objs[0].get_a() == 9 and total(objs) == sum(o.a * 10 + o.b + (o.b == 1) for o in objs)

# A property on a subclass shadows the field for its instances only.
Third.a = property(lambda self: 100)
assert objs[2].get_a() == 100 and objs[1].get_a() == 7
assert total(objs) == sum(o.a * 10 + o.b + (o.b == 1) for o in objs)
print("ok")
