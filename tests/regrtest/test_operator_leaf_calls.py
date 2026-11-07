"""`operator`'s arithmetic functions, called directly and through a
`functools.partial`, from code warm enough to take the leaf lanes: plain
ints and floats compute natively, and every other operand (bools, strs,
instances with dunders, wrong arity) keeps the full call's result or
error."""
import functools
import operator


class Num:
    def __init__(self, v):
        self.v = v

    def __add__(self, o):
        return Num(self.v + (o.v if isinstance(o, Num) else o))

    def __radd__(self, o):
        return Num(o + self.v)


add1 = functools.partial(operator.add, 1)
mul_f = functools.partial(operator.mul, 2.5)
div = functools.partial(operator.truediv, 1)
fdiv = functools.partial(operator.floordiv, 7)
mod = functools.partial(operator.mod, 7)
pw = functools.partial(operator.pow, 2)
shl = functools.partial(operator.lshift, 1)
add_kw = functools.partial(operator.add)


def run():
    out = []
    out.append(add1(2))
    out.append(operator.add(3, 4))
    out.append(add1(2**62))
    out.append(operator.mul(2**40, 2**40))
    out.append(mul_f(4))
    out.append(fdiv(-2))
    out.append(mod(-3))
    out.append(pw(-1))
    out.append(shl(70))
    out.append(add1(True))
    out.append(operator.add(True, True))
    out.append(operator.sub(1.5, 1))
    out.append(add_kw(1, 2))
    return out


expected = [3, 7, 2**62 + 1, 2**80, 10.0, -4, -2, 0.5, 2**70, 2, 2, 0.5, 3]
# Past the frame JIT's threshold, so later rounds run compiled code.
for i in range(3000):
    got = run()
    assert got == expected, got
    assert type(got[9]) is int and type(got[10]) is int

    try:
        div(0)
    except ZeroDivisionError:
        pass
    else:
        raise AssertionError("1 / 0 didn't raise")

    try:
        add1("x")
    except TypeError as e:
        assert "unsupported operand" in str(e), e
    else:
        raise AssertionError("1 + 'x' didn't raise")

    try:
        add1(1, 2)
    except TypeError:
        pass
    else:
        raise AssertionError("add(1, 1, 2) didn't raise")

    n = add1(Num(2))
    assert isinstance(n, Num) and n.v == 3
    assert operator.add("a", "b") == "ab"
    assert functools.partial(operator.add, "a")("b") == "ab"
print("ok")
