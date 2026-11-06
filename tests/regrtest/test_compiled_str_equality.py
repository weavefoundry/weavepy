"""`==` and `!=` of two strings in compiled code, settled without an
ordering (one string, two lengths, or a byte compare), keep CPython's
results: the same object, equal copies, different lengths, a difference
in the last byte, long strings, and non-ASCII text."""


def check(a, b):
    return (a == b, a != b, b == a, b != a)


def run(i):
    s = "kind%d" % (i % 7)
    t = "".join(["kind", str(i % 7)])
    assert check(s, s) == (True, False, True, False)
    assert check(s, t) == (True, False, True, False)
    assert check(s, s + "x") == (False, True, False, True)
    assert check("abcdefgh", "abcdefgi") == (False, True, False, True)
    long_a = "x" * 40 + str(i)
    long_b = "x" * 40 + str(i)
    assert check(long_a, long_b) == (True, False, True, False)
    assert check(long_a, long_b[:-1] + "!") == (False, True, False, True)
    assert check("é" * 3, "é" * 3) == (True, False, True, False)
    assert check("é", "e") == (False, True, False, True)
    assert check("", "") == (True, False, True, False)


# Past the frame JIT's threshold, so later rounds run compiled code.
for i in range(3000):
    run(i)
print("ok")
