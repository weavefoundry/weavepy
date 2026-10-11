# A method load site that sees several receiver classes keeps each one's
# method, and must still notice an instance attribute that shadows the
# method, a method replaced on its class, and a class reassigned on an
# instance.


class A:
    def __init__(self):
        self.v = 1

    def get(self):
        return "A"


class B:
    def __init__(self):
        self.v = 2

    def get(self):
        return "B"


class C(B):
    def get(self):
        return "C"


class D:
    def get(self):
        return "D"


def run(objs, n=200):
    out = []
    for _ in range(n):
        out = [o.get() for o in objs]
    return out


objs = [A(), B(), C(), D()]
assert run(objs) == ["A", "B", "C", "D"]

# An instance attribute shadows its class's method.
b2 = B()
b2.get = lambda: "shadow"
assert run([A(), b2, C(), D()]) == ["A", "shadow", "C", "D"]

# A method replaced on its class (and on a base class).
B.get = lambda self: "B2"
assert run(objs) == ["A", "B2", "C", "D"]
del C.get
assert run(objs) == ["A", "B2", "B2", "D"]

# A class reassigned on an instance.
a = A()
a.__class__ = D
assert run([a, objs[1], objs[2], objs[3]]) == ["D", "B2", "B2", "D"]

# An attribute added after the site cached the method, on an instance with
# split values of its own.
late = A()
assert run([late, objs[1]]) == ["A", "B2"]
late.get = lambda: "late"
assert run([late, objs[1]]) == ["late", "B2"]

print("test_polymorphic_method_loads: OK")
