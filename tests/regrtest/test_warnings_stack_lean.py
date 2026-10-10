"""warnings.warn attributes a warning to the right frame and line for each
stacklevel while its callers run lean or compiled (hot loops), filters
ignored warnings without side effects, and still builds the warning
object a custom category's constructor sees."""

import sys
import warnings

with open(__file__) as _f:
    _SRC = _f.read().splitlines()


def L(tag):
    marker = "# " + tag
    (line,) = [i + 1 for i, s in enumerate(_SRC) if s.endswith(marker)]
    return line


def inner(level):
    warnings.warn("w%d" % level, UserWarning, level)  # in-warn
    return 0


def mid(level):
    return inner(level)  # mid-call


def outer(level):
    return mid(level)  # outer-call


def collect(n, level):
    with warnings.catch_warnings(record=True) as got:
        warnings.simplefilter("always")
        for i in range(n):
            outer(level)  # collect-call
    return {(w.filename == __file__, w.lineno, str(w.message)) for w in got}


made = []


class Custom(UserWarning):
    def __init__(self, *args):
        made.append(args)
        super().__init__(*args)


def ignored(n):
    with warnings.catch_warnings():
        warnings.simplefilter("ignore")
        for i in range(n):
            warnings.warn("x", DeprecationWarning)
            warnings.warn("y", Custom)
            warnings.warn("z", UserWarning, stacklevel=2)
    return len(made)


def run(n):
    for level, tag in ((1, "in-warn"), (2, "mid-call"), (3, "outer-call"), (4, "collect-call")):
        got = collect(n, level)
        assert got == {(True, L(tag), "w%d" % level)}, (level, got)
    made.clear()
    assert ignored(n) == n
    with warnings.catch_warnings():
        warnings.simplefilter("error")
        try:
            inner(1)
        except UserWarning as e:
            assert str(e) == "w1" and type(e) is UserWarning
        else:
            raise AssertionError("no error")


run(5)
run(3000)
run(3000)
