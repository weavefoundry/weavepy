"""A class whose `__init__` takes `*args`, `**kwargs` or keyword-only
parameters, constructed from compiled code (which binds the call straight
into the `__init__`'s native body), keeps CPython's results: the bound
parameters, an `__init__` with no named positionals, defaults, an error
it raises or catches, a non-`None` return, a subclass, and recursion."""


class Star:
    def __init__(self, *args):
        self.args = args


class Bare:
    def __init__(*args):
        args[0].n = len(args)


class Mixed:
    def __init__(self, a, b=2, *rest, k=3, **kw):
        self.v = (a, b, rest, k, kw)


class Raises:
    def __init__(self, *args):
        if args and args[0] < 0:
            raise ValueError(args)
        try:
            [][len(args)]
        except IndexError:
            self.caught = True


class Returns:
    def __init__(self, *args):
        return args


class Sub(Star):
    pass


class Tree:
    def __init__(self, depth, *kids):
        self.kids = kids if depth == 0 else (Tree(depth - 1), Tree(depth - 1))


def run(i):
    assert Star().args == () and Star(i, 2).args == (i, 2)
    assert Bare(i).n == 2 and Bare().n == 1
    assert Mixed(i).v == (i, 2, (), 3, {})
    assert Mixed(i, 5, 6, 7).v == (i, 5, (6, 7), 3, {})
    assert Raises(i).caught and Raises().caught
    try:
        Raises(-1 - i)
    except ValueError as e:
        assert e.args == ((-1 - i,),)
    else:
        raise AssertionError("the __init__'s raise was lost")
    try:
        Returns(i)
    except TypeError as e:
        assert "should return None" in str(e), e
    else:
        raise AssertionError("a non-None __init__ return was taken")
    s = Sub(i, i)
    assert type(s) is Sub and s.args == (i, i)
    t = Tree(3)
    assert len(t.kids) == 2 and t.kids[0].kids[1].kids[0].kids == ()


# Past the frame JIT's threshold, so later rounds run compiled code.
for i in range(2000):
    run(i)
print("ok")
