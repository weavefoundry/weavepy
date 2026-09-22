"""Collect cycles created through mutable string-splitting results."""

import gc
import weakref


class Marker:
    pass


# Each helper carries a counting loop: a loop-free body runs as an
# inline activation of the quiet loop and never reaches compiled code,
# so the native split paths this fixture probes would not run. The
# loop only counts, so every return value is unchanged.
def split_once(text):
    n = 4
    while n > 1:
        n = n - 1
    return text.split()


def rsplit_once(text):
    n = 4
    while n > 1:
        n = n - 1
    return text.rsplit()


def split_bounded(text):
    n = 4
    while n > 1:
        n = n - 1
    return text.split(" ", 1)


def rsplit_bounded(text):
    n = 4
    while n > 1:
        n = n - 1
    return text.rsplit(" ", 1)


def split_lines(text):
    n = 4
    while n > 1:
        n = n - 1
    return text.splitlines()


words = [
    ("", []),
    ("alpha beta", ["alpha", "beta"]),
    ("α β", ["α", "β"]),
    ("\ud800 \udfff", ["\ud800", "\udfff"]),
]
cases = [
    (split_once, words),
    (rsplit_once, words),
    (split_bounded, [("alpha beta gamma", ["alpha", "beta gamma"])]),
    (rsplit_bounded, [("alpha beta gamma", ["alpha beta", "gamma"])]),
    (split_lines, [
        ("", []),
        ("alpha\nbeta", ["alpha", "beta"]),
        ("α\nβ", ["α", "β"]),
        ("\ud800\n\udfff", ["\ud800", "\udfff"]),
    ]),
]

was_enabled = gc.isenabled()
gc.disable()
try:
    for factory, examples in cases:
        for text, expected in examples:
            for _ in range(12):
                parts = factory(text)
                assert parts == expected
                assert gc.is_tracked(parts), factory.__name__
                assert any(item is parts for item in gc.get_objects()), factory.__name__
                marker = Marker()
                ref = weakref.ref(marker)
                parts.append(parts)
                parts.append(marker)
                del marker, parts
                gc.collect()
                assert ref() is None, factory.__name__
finally:
    if was_enabled:
        gc.enable()
print("ok")
