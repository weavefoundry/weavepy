"""Attribute loads on `super()` proxies resolve along the receiver's MRO
past the proxy's class at every execution, never along the `super` type's
own MRO, including names `object` also defines (`__new__`, `__init__`,
`__eq__`). This module names `super` at top level, so the compiler emits
the generic `super()` call and an ordinary `LOAD_ATTR` (as CPython does)."""

the_super = super


class Base:
    def __new__(cls, *args):
        return the_super(Base, cls).__new__(cls)

    def __init__(self, *args):
        self.args = args

    def __eq__(self, other):
        return "Base.__eq__"

    def m(self):
        return "Base.m"


class Sub(Base):
    def __new__(cls, *args):
        return super().__new__(cls, *args)

    def __init__(self, *args):
        super().__init__(*args)

    def __eq__(self, other):
        return super().__eq__(other)

    def m(self):
        return super().m()


class Sub2(Base):
    def __new__(cls, *args):
        proxy = super()
        return proxy.__new__(cls, *args)

    def __init__(self, *args):
        proxy = super()
        proxy.__init__(*args)


for i in range(300):
    s = Sub(i)
    assert type(s) is Sub and s.args == (i,)
    assert (s == 1) == "Base.__eq__"
    assert s.m() == "Base.m"
    s2 = Sub2(i, i)
    assert type(s2) is Sub2 and s2.args == (i, i)


# A `super` subclass as the proxy.
class MySuper(super):
    pass


class Sub3(Base):
    def __init__(self, *args):
        MySuper(Sub3, self).__init__(*args)


for i in range(300):
    assert Sub3(i).args == (i,)

print("ok")
