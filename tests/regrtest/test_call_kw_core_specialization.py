"""Keyword call sites specialized from the core loop keep exact binding
semantics: a site that changes callee, defaults replaced after the site
specialized, binder errors, keyword-only and `**kwargs` callees, and
argument lifetimes."""

import gc


def run(fn, n=300):
    for _ in range(n):
        fn()


# One site, a callee that changes shape between executions.
def f_ab(a, b=2, c=3):
    return ("ab", a, b, c)


def f_ba(a, c=30, b=20):
    return ("ba", a, b, c)


def f_kwargs(a, **kw):
    return ("kw", a, sorted(kw.items()))


def f_kwonly(a, *, b, c=7):
    return ("ko", a, b, c)


def f_args(a, *rest, b=0, c=0):
    return ("args", a, rest, b, c)


def call_site(f):
    return f(1, b=2, c=3)


expected = {
    f_ab: ("ab", 1, 2, 3),
    f_ba: ("ba", 1, 2, 3),
    f_kwargs: ("kw", 1, [("b", 2), ("c", 3)]),
    f_kwonly: ("ko", 1, 2, 3),
    f_args: ("args", 1, (), 2, 3),
}
order = [f_ab] * 50 + [f_ba] * 50 + [f_kwargs] * 50 + [f_kwonly] * 50 + [f_args] * 50
for _ in range(4):
    for f in order:
        assert call_site(f) == expected[f], (f, call_site(f))
    order.reverse()


# Defaults replaced after the site has run many times.
def g(a, b=2, c=3):
    return (a, b, c)


def call_g():
    return g(1, b=5)


for _ in range(300):
    assert call_g() == (1, 5, 3)
g.__defaults__ = (20, 30)
for _ in range(50):
    assert call_g() == (1, 5, 30)
g.__defaults__ = None
for _ in range(50):
    try:
        call_g()
    except TypeError as e:
        assert "missing 1 required positional argument: 'c'" in str(e), str(e)
    else:
        raise AssertionError("expected TypeError")
g.__defaults__ = (2, 3)
g.__kwdefaults__ = None
for _ in range(50):
    assert call_g() == (1, 5, 3)


# Binder errors keep CPython's messages at a warm site.
def h(a, b=2):
    return (a, b)


def bad_dup():
    return h(1, a=2)


def bad_name():
    return h(1, z=2)


for _ in range(200):
    assert h(1, b=3) == (1, 3)
for fn, msg in [
    (bad_dup, "h() got multiple values for argument 'a'"),
    (bad_name, "h() got an unexpected keyword argument 'z'"),
]:
    for _ in range(100):
        try:
            fn()
        except TypeError as e:
            assert str(e) == msg, str(e)
        else:
            raise AssertionError("expected TypeError")


# Positional-only parameters can't bind by keyword; `**kwargs` collects
# the name instead.
def po(a, /, **kw):
    return (a, kw)


def call_po():
    return po(1, a=2)


for _ in range(300):
    assert call_po() == (1, {"a": 2})


# A keyword naming the sixteenth parameter slot (as
# `dataclasses.make_dataclass(..., decorator=...)` does).
def sixteen(p0, p1, *, k2=2, k3=3, k4=4, k5=5, k6=6, k7=7, k8=8, k9=9,
            k10=10, k11=11, k12=12, k13=13, k14=14, last=None):
    return (p0, p1, k14, last)


def call_sixteen():
    return sixteen(0, 1, k14="a", last="b")


for _ in range(300):
    assert call_sixteen() == (0, 1, "a", "b")


# Methods called with keywords through the elided self slot.
class K:
    def m(self, a, b=0, c=0):
        return (self.tag, a, b, c)

    def __init__(self, tag):
        self.tag = tag


ks = [K(i) for i in range(3)]


def call_m(k):
    return k.m(1, c=3)


for _ in range(200):
    for k in ks:
        assert call_m(k) == (k.tag, 1, 0, 3)


# A keyword argument's last reference dies with the call.
class Tracked:
    alive = 0

    def __init__(self):
        Tracked.alive += 1

    def __del__(self):
        Tracked.alive -= 1


def take(a, b=None):
    return a


def call_take():
    take(1, b=Tracked())
    return Tracked.alive


gc.disable()
try:
    for _ in range(300):
        assert call_take() == 0
finally:
    gc.enable()

print("ok")
