"""Integer arithmetic past the machine word from compiled code (and the
core loop) keeps CPython's results: products, sums and differences that
overflow into big ints, big ints with small ones, the in-place forms,
and the errors (a division by zero, a negative shift), raised as the
full handler raises them."""

M = 2**62


def check(i):
    a, b = M + i, 3
    assert a * b == (M + i) * 3 and a * b > 2**63
    assert a + a == 2 * (M + i) and -a - a == -2 * (M + i)
    big = a * a
    assert big // a == a and big % a == 0 and big - big == 0
    assert (big + 1) * 2 == 2 * big + 2 and big * -1 == -big
    assert 1 - big == -(big - 1) and big ** 1 == big
    t = 0
    t += big
    t *= 2
    assert t == 2 * big
    assert (a << 3) == (M + i) * 8 and big >> 64 == big // 2**64
    assert big & 0xFF == ((M + i) ** 2) % 256 and (big | 1) >= big
    assert big / a == float(a) and a * 0.5 == (M + i) / 2
    for bad in (lambda: big // 0, lambda: big % 0, lambda: a << -1):
        try:
            bad()
        except (ZeroDivisionError, ValueError):
            pass
        else:
            raise AssertionError("an error didn't raise")
    assert True + M * 2 == 2 * M + 1 and type(big * True) is int


# Past the frame JIT's threshold, so later rounds run compiled code.
for i in range(3000):
    check(i)
print("ok")
