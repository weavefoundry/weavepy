"""`operator.itemgetter`, `attrgetter`, and `methodcaller` calls.

WeavePy calls their native `__call__` straight from the dispatch loop,
subscripting plain lists, tuples, and dicts in place; every other shape
and every error goes through the full call. A `functools.partial` with
at most one stored argument is called as its function. These checks pin
the results and errors of both paths, repeated so the loop's caches warm.
"""

import functools
import operator


def raises(exc, fn, *args):
    try:
        fn(*args)
    except exc as e:
        return e
    raise AssertionError("%s not raised" % exc.__name__)


class Seq:
    def __getitem__(self, i):
        return ("seq", i)


class Obj:
    def __init__(self):
        self.a = 1
        self.b = self

    def m(self, x, y=0):
        return x + y


ig = operator.itemgetter(1)
igd = operator.itemgetter("k")
ig2 = operator.itemgetter(-1, 0)
for _ in range(50):
    assert ig((5, 6)) == 6 and ig([7, 8]) == 8 and ig("ab") == "b"
    assert ig(Seq()) == ("seq", 1)
    assert igd({"k": 3}) == 3 and igd({"k": None}) is None
    assert ig2((1, 2, 3)) == (3, 1)
    assert isinstance(raises(IndexError, ig, (1,)), IndexError)
    assert raises(KeyError, igd, {}).args == ("k",)
    raises(TypeError, ig, 5)

ag = operator.attrgetter("a")
agb = operator.attrgetter("b.a", "a")
mc = operator.methodcaller("m", 2, y=3)
o = Obj()
for _ in range(50):
    assert ag(o) == 1 and agb(o) == (1, 1) and mc(o) == 5
    raises(AttributeError, ag, 5)
raises(TypeError, operator.attrgetter, 1)
assert repr(ig2) == "operator.itemgetter(-1, 0)"
assert repr(agb) == "operator.attrgetter('b.a', 'a')"
assert sorted([{"k": 2}, {"k": 1}], key=igd) == [{"k": 1}, {"k": 2}]


def f(a, b=10):
    return a * b


p0 = functools.partial(f)
p1 = functools.partial(f, 3)
pk = functools.partial(f, b=2)
total = 0
for i in range(50):
    total += p0(i) + p1(i) + pk(i)
    assert p1() == 30
raises(TypeError, p1, 1, 2)
assert total == sum(10 * i + 3 * i + 2 * i for i in range(50))

print("operator getters native ok")
