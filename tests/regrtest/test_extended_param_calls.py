"""Calls of functions with *args, **kwargs, and keyword-only parameters,
and f(*args) spreads, bind exactly as the full binder does."""


def star(*args):
    return args


def mixed(a, b=2, *rest, flag=False, **kw):
    return a, b, rest, flag, kw


def kwonly(a, *, sep="-", end="!"):
    return sep.join(a) + end


def required(a, *, need):
    return a, need


def fwd(*args):
    return mixed(*args)


def fwd_kwonly(*args):
    return kwonly(*args)


def counter(*args):
    count = 0
    for _ in args:
        count += 1
    return count


def make_adder(n):
    def add(*xs):
        return n + sum(xs)
    return add


class Box:
    def method(self, *args, scale=1):
        return tuple(x * scale for x in args)


EMPTY = ()
box = Box()
add3 = make_adder(3)
dicts = []
for i in range(2000):
    assert star() == () and star() is EMPTY
    assert star(i) == (i,)
    assert star(i, i + 1, "x") == (i, i + 1, "x")
    a, b, rest, flag, kw = mixed(i)
    assert (a, b, rest, flag, kw) == (i, 2, (), False, {})
    dicts.append(kw)
    assert mixed(i, 5, 6, 7) == (i, 5, (6, 7), False, {})
    assert kwonly("ab") == "a-b!"
    assert fwd(i, 1, 2) == (i, 1, (2,), False, {})
    assert fwd(i) == (i, 2, (), False, {})
    assert fwd_kwonly("xyz") == "x-y-z!"
    assert counter(*range(i % 7)) == i % 7
    assert counter(*(1, 2, 3)) == 3
    assert add3(i, 1) == i + 4
    assert box.method(i, 2) == (i, 2)
    assert Box.method(box, i) == (i,)
    spread = (i, 1)
    assert mixed(*spread) == (i, 1, (), False, {})
    assert star(*[i, i]) == (i, i)
    if i % 500 == 0:
        try:
            required("a")
        except TypeError as e:
            assert "missing 1 required keyword-only argument: 'need'" in str(e), e
        else:
            raise AssertionError("missing keyword-only argument accepted")
        try:
            kwonly("a", "b")
        except TypeError as e:
            assert "takes 1 positional argument but 2 were given" in str(e), e
        else:
            raise AssertionError("extra positional argument accepted")
        try:
            fwd()
        except TypeError as e:
            assert "missing 1 required positional argument: 'a'" in str(e), e
        else:
            raise AssertionError("missing positional argument accepted")


def fwd_all(*args, **kwargs):
    return mixed(*args, **kwargs)


def posonly(a, /, b, **kw):
    return a, b, kw


class Wrapper:
    __slots__ = ("func",)

    def __init__(self, func):
        self.func = func

    def __call__(self, *args, **kwargs):
        return self.func(*args, **kwargs)


class Plain:
    def __call__(self, x, y=1):
        return x * y


wrapped = Wrapper(mixed)
plain = Plain()
for i in range(2000):
    assert fwd_all(i, flag=True) == (i, 2, (), True, {})
    assert fwd_all(i, b=3, extra=4) == (i, 3, (), False, {"extra": 4})
    assert fwd_all(a=i) == (i, 2, (), False, {})
    assert fwd_all(i, 5, 6, z=1, y=2) == (i, 5, (6,), False, {"z": 1, "y": 2})
    assert list(fwd_all(i, q=1, p=2)[4]) == ["q", "p"]
    assert posonly(i, b=2, a=3) == (i, 2, {"a": 3})
    assert posonly(*(i, 1), **{"a": 5}) == (i, 1, {"a": 5})
    assert wrapped(i) == (i, 2, (), False, {})
    assert wrapped(i, flag=1) == (i, 2, (), 1, {})
    assert plain(i) == i and plain(i, 3) == 3 * i and plain(i, y=2) == 2 * i
    if i % 500 == 0:
        try:
            fwd_all(i, a=1)
        except TypeError as e:
            assert "multiple values for argument 'a'" in str(e), e
        else:
            raise AssertionError("duplicate argument accepted")
        try:
            kwonly(*("ab",), **{"nope": 1})
        except TypeError as e:
            assert "unexpected keyword argument 'nope'" in str(e), e
        else:
            raise AssertionError("unexpected keyword accepted")
        try:
            plain()
        except TypeError as e:
            assert "missing 1 required positional argument: 'x'" in str(e), e
        else:
            raise AssertionError("missing argument accepted")

Plain.__call__ = lambda self, x, y=1: x + y
assert plain(2, 3) == 5
del Plain.__call__
try:
    plain(1)
except TypeError as e:
    assert "not callable" in str(e), e
else:
    raise AssertionError("uncallable instance called")

# A finalizable argument whose last reference goes with a spread call's
# operands is finalized when the call returns.
FINALIZED = []


class Finalized:
    def __del__(self):
        FINALIZED.append(1)


def sink(obj=None, **kw):
    return None


def spread(*args, **kwargs):
    return sink(*args, **kwargs)


class Keeper:
    def take(self, *args, **kwargs):
        return None


keeper = Keeper()
for i in range(200):
    del FINALIZED[:]
    spread(Finalized(), flag=i)
    assert FINALIZED == [1], FINALIZED
    spread(obj=Finalized())
    assert FINALIZED == [1, 1], FINALIZED
    spread(i, other=Finalized())
    assert FINALIZED == [1, 1, 1], FINALIZED
    sink(*(Finalized(),), **{"x": 1})
    assert FINALIZED == [1, 1, 1, 1], FINALIZED
    # Only the `**` mapping holds it (pickle's `save_reduce(obj=obj, *rv)`).
    sink(*(), obj=Finalized())
    assert FINALIZED == [1, 1, 1, 1, 1], FINALIZED
    keeper.take(*(i,), obj=Finalized())
    assert FINALIZED == [1, 1, 1, 1, 1, 1], FINALIZED

# Every call's **kwargs is its own dictionary.
assert len({id(d) for d in dicts}) == len(dicts)
dicts[0]["x"] = 1
assert dicts[1] == {}

# Replaced defaults take effect at once.
kwonly.__kwdefaults__ = {"sep": "+", "end": "?"}
assert kwonly("ab") == "a+b?"
kwonly.__kwdefaults__ = None
try:
    kwonly("ab")
except TypeError as e:
    assert "keyword-only" in str(e), e
else:
    raise AssertionError("cleared __kwdefaults__ ignored")
mixed.__defaults__ = (9,)
assert mixed(1) == (1, 9, (), False, {})


def deep(n, *args):
    return deep(n + 1, *args)


try:
    deep(0, 1)
except RecursionError:
    pass
else:
    raise AssertionError("unbounded recursion")
print("ok")
