"""Compiled code's method calls on ints, bools, floats, tuples and bytes
keep CPython's results, including a site that meets several kinds of
receiver (a bool where an int was, a big int, a subclass instance) and a
method one kind lacks."""


class MyInt(int):
    def bit_length(self):
        return -1


def calls(xs):
    out = []
    for x in xs:
        out.append(x.bit_length())
    return out


assert calls([7] * 3000)[-1] == 3
assert calls([7, True, False, 1 << 80, MyInt(5), -8]) == [3, 1, 0, 81, -1, 4]


def mixed(n):
    t = 0
    for i in range(n):
        f = i + 0.5
        t += f.as_integer_ratio()[1] + (i, i, 1).count(i) + len(b"ab".decode()) + f.is_integer()
    return t


assert mixed(3000) == sum(2 + (3 if i == 1 else 2) + 2 for i in range(3000))


def bad(v):
    try:
        return v.decode()
    except AttributeError:
        return "no"


for _ in range(3000):
    assert bad(b"x") == "x"
assert bad(3) == "no" and bad((1,)) == "no"
print("ok")
