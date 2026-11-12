"""Builtins called with keywords from compiled code whose call runs
Python code (`sorted(..., key=lambda ...)`, `max(..., key=...)`) keep
CPython's results: the key runs per item, an error the key raises reaches
the caller's handler, and a later raise in the caller still has its
frame."""
import sys


class Boom(Exception):
    pass


def key_raises(v):
    if v == 3:
        raise Boom(v)
    return -v


def run(i):
    xs = (i % 7, i % 3, i % 5)
    assert sorted(xs, key=lambda v: -v) == sorted(xs, reverse=True)
    assert max(xs, key=lambda v: (v, -i)) == max(xs)
    assert sorted([3, 1, 2], key=lambda v: v, reverse=True) == [3, 2, 1]
    try:
        sorted([1, 2, 3], key=key_raises)
    except Boom as e:
        assert e.args == (3,)
    else:
        raise AssertionError("the key's raise was lost")


for i in range(3000):
    run(i)


def outer():
    try:
        return sorted([2, 1], key=lambda v: 1 / 0)
    except ZeroDivisionError:
        return sys._getframe().f_code.co_name


assert outer() == "outer"
print("ok")
