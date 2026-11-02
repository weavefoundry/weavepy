"""A surplus positional argument to a warm pure leaf raises TypeError.

Native callers (a spread call the core loop hands to the ordinary call,
`map`, `sorted`'s key) evaluate a warm pure leaf in place, binding the
arguments by position. A function with keyword-only parameters has more
slots than positional parameters, and a surplus positional used to land
in the first keyword-only one (`f(1, 2, 3)` of `def f(w, h=1, *, scale=1.0)`
returned 6) instead of raising, as it does when the function is cold.
"""


def plain(w, h=1, *, scale=1.0):
    return w * h * scale


def one(w, *, scale=1.0):
    return w * scale


def wrapped(fn):
    def wrapper(*args, **kwargs):
        return fn(*args, **kwargs)

    return wrapper


area = wrapped(plain)

for i in range(3000):
    assert plain(i) == i
    assert area(i, h=2) == 2 * i
    assert sorted([3, 1], key=one) == [1, 3]

for call in (
    lambda: area(1, 2, 3),
    lambda: plain(*(1, 2, 3)),
    lambda: list(map(one, [1], [2])),
):
    try:
        call()
    except TypeError as e:
        assert "positional argument" in str(e), e
    else:
        raise AssertionError("a surplus positional argument bound")

print("ok")
