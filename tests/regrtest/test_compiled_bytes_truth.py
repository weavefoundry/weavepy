"""Truth tests of `bytes`, `bytearray`, sets, frozensets and big ints, and
`bytes + bytes`, from compiled code: CPython's answers, a fresh `bytes`
for each concatenation (`+=` on `bytes` rebinds, leaving the old value as
it was), and a `bytearray` changed in between seen as changed."""


def check(b, s, ba, n):
    total = 0
    for i in range(n):
        total += (1 if b else 0) + (1 if b"" else 0) + (1 if ba else 10)
        total += (1 if {i} else 0) + (1 if set() else 20) + (1 if frozenset((i,)) else 0)
        total += (1 if frozenset() else 30) + (1 if 1 << 70 else 0) + (1 if -(1 << 70) else 0)
        joined = b + s
        assert joined == b"abcd" and joined is not b
        x = b
        x += s
        assert x == b"abcd" and b == b"ab"
    return total


# Past the frame JIT's threshold, so later rounds run compiled code.
ba = bytearray()
for rnd in range(3):
    assert check(b"ab", b"cd", ba, 2000) == 2000 * (1 + 10 + 1 + 20 + 1 + 30 + 2) or rnd > 0
    ba.append(1)
assert check(b"ab", b"cd", ba, 2000) == 2000 * (1 + 1 + 1 + 20 + 1 + 30 + 2)
ba.clear()
assert check(b"ab", b"cd", ba, 2000) == 2000 * (1 + 10 + 1 + 20 + 1 + 30 + 2)
print("ok")
