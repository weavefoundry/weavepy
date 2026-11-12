"""Python callbacks a native caller runs (`map`, `sorted(key=)`, `min`,
`filter`), whose compiled bodies are entered straight from the callback,
keep CPython's results: their returns, an exception they catch, one they
raise (with its traceback through the callback's frame), a frame they
look at, a call they make, and recursion."""
import sys
import traceback


def ratio(x):
    try:
        return x.as_integer_ratio()
    except AttributeError:
        return (x, 1)


def key(x):
    return (-len(str(x)), str(x))


def boom(x):
    if x == 7:
        raise ValueError("seven")
    return x


def caller_name(x):
    return sys._getframe(1).f_code.co_name, x


def depth(x):
    return 0 if x <= 0 else 1 + depth(x - 1)


def check(i):
    xs = [0.5, 1.25, 3, "s"]
    assert list(map(ratio, xs)) == [(1, 2), (5, 4), (3, 1), ("s", 1)]
    assert sorted([3, 10, 200, 7], key=key) == [200, 10, 3, 7]
    assert min([5, 9, 1], key=lambda v: -v) == 9
    assert list(filter(lambda v: v % 2, range(6))) == [1, 3, 5]
    assert list(map(depth, [0, 3, 5])) == [0, 3, 5]
    try:
        list(map(boom, range(10)))
    except ValueError as e:
        names = [f.name for f in traceback.extract_tb(e.__traceback__)]
        assert names[-1] == "boom", names
    else:
        raise AssertionError("the callback's raise was lost")
    assert list(map(caller_name, [1]))[0][1] == 1


# Past the frame JIT's threshold, so later rounds run compiled code.
for i in range(2000):
    check(i)
print("ok")
