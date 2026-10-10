"""`sys._getframe` materializes one frame and links its `f_back` chain
lazily, a returning activation with a frame object settles without the
general loop, and the native `posix._path_normpath`/`_path_splitroot_ex`,
`time.localtime`/`strftime` and module `hasattr` paths keep CPython's
observable behavior."""

import os
import posixpath
import sys
import time


# --- Lazily linked frames ----------------------------------------------

def walk(depth):
    f = sys._getframe(depth)
    out = []
    while f is not None:
        out.append((f.f_code.co_name, f.f_lineno))
        f = f.f_back
    return out


def level1():
    return walk(1)


def level2():
    return level1()


def level3():
    return level2()


expected = None
for i in range(500):
    got = level3()
    if expected is None:
        expected = got
    assert got == expected, (i, got, expected)
assert [n for n, _ in expected[:3]] == ["level1", "level2", "level3"], expected


def keep_caller():
    return sys._getframe(1)


def holder():
    fr = keep_caller()
    first = fr.f_lineno
    x = 1
    second = fr.f_lineno
    back = fr.f_back
    return first, second, back.f_code.co_name, fr.f_code.co_name, x


for i in range(500):
    first, second, back_name, name, _ = holder()
    assert second == first + 2, (first, second)
    assert back_name == "<module>" and name == "holder"


def own_frame():
    f = sys._getframe()
    a = 1
    line_a = f.f_lineno
    b = 2
    line_b = f.f_lineno
    return line_a, line_b, f.f_back.f_code.co_name


for i in range(500):
    la, lb, caller = own_frame()
    assert lb == la + 2 and caller == "<module>"


def escaping():
    def inner():
        return sys._getframe(1)
    local_value = 42
    return inner()


for i in range(300):
    fr = escaping()
    assert fr.f_code.co_name == "escaping"
    assert fr.f_locals["local_value"] == 42
    assert fr.f_back is not None and fr.f_back.f_code.co_name == "<module>"


def callee_of_departed():
    return sys._getframe(0)


def departed_caller():
    return callee_of_departed()


for i in range(300):
    fr = departed_caller()
    # The caller left the stack while the callee's frame lived: its frame
    # object was built on the way out.
    assert fr.f_back.f_code.co_name == "departed_caller"
    assert fr.f_back.f_back.f_code.co_name == "<module>"


def gen_frames():
    yield sys._getframe(0).f_back.f_code.co_name
    yield sys._getframe(1).f_code.co_name


def consume():
    return list(gen_frames())


for i in range(200):
    assert consume() == ["consume", "consume"]

assert sys._getframemodulename(0) == "__main__"


# --- posixpath accelerators ---------------------------------------------

def py_splitroot(p):
    p = os.fspath(p)
    sep, empty = (b"/", b"") if isinstance(p, bytes) else ("/", "")
    if p[:1] != sep:
        return empty, empty, p
    elif p[1:2] != sep or p[2:3] == sep:
        return empty, sep, p[1:]
    else:
        return empty, p[:2], p[2:]


def py_normpath(path):
    path = os.fspath(path)
    if isinstance(path, bytes):
        sep, dot, dotdot = b"/", b".", b".."
    else:
        sep, dot, dotdot = "/", ".", ".."
    if not path:
        return dot
    _, initial_slashes, path = py_splitroot(path)
    new_comps = []
    for comp in path.split(sep):
        if not comp or comp == dot:
            continue
        if (comp != dotdot or (not initial_slashes and not new_comps)
                or (new_comps and new_comps[-1] == dotdot)):
            new_comps.append(comp)
        elif new_comps:
            new_comps.pop()
    path = initial_slashes + sep.join(new_comps)
    return path or dot


cases = ["", ".", "..", "/", "//", "///", "////a", "//a/b", "a/b/../..",
         "../a", "/../a", "a//b/./c/..", "/a/./b/../c/5", "./", "a/b/",
         "..//..", "/..", "x/../../y", "é/./ü/..", "a\x00b/../c",
         "/a/\udcff/../b", "//\udcff", "a/../../b/./c//"]
for c in cases:
    assert posixpath.normpath(c) == py_normpath(c), c
    assert posixpath.splitroot(c) == py_splitroot(c), c
    if "\udcff" not in c:
        b = c.encode()
        assert posixpath.normpath(b) == py_normpath(b), c
        assert posixpath.splitroot(b) == py_splitroot(b), c


class P:
    def __fspath__(self):
        return "/a/./b/../c"


assert posixpath.normpath(P()) == "/a/c"
assert posixpath.splitroot(P()) == ("", "/", "a/./b/../c")
for bad in (1, None, [1]):
    for fn in (posixpath.normpath, posixpath.splitroot):
        try:
            fn(bad)
        except TypeError:
            pass
        else:
            raise AssertionError((fn, bad))


# --- time.struct_time and strftime ------------------------------------

t = time.localtime(1700000000.5)
assert len(t) == 9 and tuple(t)[:3] == (t.tm_year, t.tm_mon, t.tm_mday)
assert isinstance(t.tm_zone, str) and isinstance(t.tm_gmtoff, int)
assert time.strftime("%Y", t) == str(t.tm_year)
assert time.strftime("", t) == ""
assert time.strftime("café %Y", t) == "café " + str(t.tm_year)
assert time.strftime("%Y-%m-%d", t) == "%04d-%02d-%02d" % (t.tm_year, t.tm_mon, t.tm_mday)
assert time.mktime(t) == 1700000000.0


# --- hasattr / getattr on modules -------------------------------------

assert hasattr(os, "getpid") and not hasattr(os, "no_such_name")
assert getattr(os, "sep") == "/" and getattr(os, "no_such_name", 5) == 5
assert hasattr(os, "__dict__") and getattr(os, "__name__") == "os"
