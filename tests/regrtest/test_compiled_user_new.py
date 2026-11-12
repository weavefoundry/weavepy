"""A class with a Python `__new__`, constructed from compiled code (which
enters `__new__`'s native body directly and finishes the construction on
its return), keeps CPython's results: `__init__` after `__new__` with the
same arguments, none when `__init__` is `object`'s, a result of another
type (no `__init__`), a subclass instance with an `__init__` of its own,
a raise in either, `*args` and defaults, and a singleton."""
import traceback


class Plain:
    def __new__(cls, a, b=2):
        obj = object.__new__(cls)
        obj.ab = (a, b)
        return obj


class WithInit:
    def __new__(cls, *args, **kw):
        return super().__new__(cls)

    def __init__(self, a, b=5, *, c=7):
        self.v = (a, b, c)


class Other:
    def __new__(cls, x):
        return x * 2


class Sub(Plain):
    def __init__(self, a, b=2):
        self.sub = a


class Picky:
    def __new__(cls, a):
        return Sub(a) if a < 0 else object.__new__(cls)


class Raises:
    def __new__(cls, a):
        if a == 3:
            raise ValueError(a)
        return object.__new__(cls)


class InitRaises:
    def __new__(cls, a):
        return object.__new__(cls)

    def __init__(self, a):
        if a == 4:
            raise KeyError(a)


class Single:
    _it = None

    def __new__(cls):
        if cls._it is None:
            cls._it = object.__new__(cls)
        return cls._it


def run(i):
    p = Plain(i)
    assert type(p) is Plain and p.ab == (i, 2) and Plain(i, 9).ab == (i, 9)
    w = WithInit(i, c=1)
    assert w.v == (i, 5, 1) and WithInit(i, 3).v == (i, 3, 7)
    assert Other(i) == 2 * i
    s = Picky(-1 - i % 3)
    assert type(s) is Sub and s.sub == -1 - i % 3 and s.ab == (-1 - i % 3, 2)
    assert type(Picky(i)) is Picky and not hasattr(Picky(i), "sub")
    try:
        Raises(3)
    except ValueError as e:
        assert [f.name for f in traceback.extract_tb(e.__traceback__)][-1] == "__new__"
    else:
        raise AssertionError("__new__'s raise was lost")
    assert type(Raises(i % 3)) is Raises
    try:
        InitRaises(4)
    except KeyError:
        pass
    else:
        raise AssertionError("__init__'s raise was lost")
    assert type(InitRaises(i % 4)) is InitRaises
    assert Single() is Single()
    try:
        Plain()
    except TypeError as e:
        assert "missing 1 required positional argument: 'a'" in str(e), e
    else:
        raise AssertionError("a missing argument was taken")


# Past the frame JIT's threshold, so later rounds run compiled code.
for i in range(2000):
    run(i)
print("ok")
