"""RFC 0056 — TypeError message shape for too-many-positionals + kwonly.

CPython's ``too_many_positional`` switches to a longer form when any
keyword-only argument is also supplied: the positional count is
labelled explicitly and the kwonly count is parenthetical. ``test_extcall``
asserts these strings literally.
"""


def check(fn, args, kwargs, expected):
    try:
        fn(*args, **kwargs)
    except TypeError as exc:
        assert str(exc) == expected, (str(exc), expected)
    else:
        raise AssertionError(f"expected TypeError: {expected}")


def f0():
    pass


check(f0, (1,), {}, "f0() takes 0 positional arguments but 1 was given")


def f1(a):
    pass


check(f1, (1, 2), {}, "f1() takes 1 positional argument but 2 were given")


def f2(a, b=1):
    pass


check(
    f2,
    (1, 2, 3),
    {},
    "f2() takes from 1 to 2 positional arguments but 3 were given",
)


def f3(*, kw):
    pass


check(
    f3,
    (1,),
    {"kw": 3},
    "f3() takes 0 positional arguments but 1 positional argument "
    "(and 1 keyword-only argument) were given",
)


def f4(*, kw, b):
    pass


check(
    f4,
    (1, 2, 3),
    {"b": 3, "kw": 3},
    "f4() takes 0 positional arguments but 3 positional arguments "
    "(and 2 keyword-only arguments) were given",
)


def f5(a, b=2, *, kw):
    pass


check(
    f5,
    (2, 3, 4),
    {"kw": 4},
    "f5() takes from 1 to 2 positional arguments but 3 positional arguments "
    "(and 1 keyword-only argument) were given",
)

print("extcall kwonly messages ok")
