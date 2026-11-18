"""`bytes * n` built in the result's own storage.

One byte repeats as a fill, longer units by doubling copies; `b * 1` is
`b` itself, and an empty unit or a count of zero or less gives empty
bytes. Run it past the compile threshold, with sizes on both sides
of the compiled code's in-line limit.
"""


def run(n):
    for i in range(n):
        unit = b"ab" if i % 2 else b"x"
        k = i % 9
        r = unit * k
        assert r == bytes(unit[j % len(unit)] for j in range(len(unit) * k))
        assert (k * unit) == r
        b = b"xyz"
        assert b * 1 is b
        assert b * 0 == b"" and b"" * k == b"" and b * -3 == b""
    big = b"0123456789" * 300_000
    assert len(big) == 3_000_000 and big[-11:] == b"90123456789"
    assert big.count(b"0123456789") == 300_000
    assert b"\xff" * (1 << 21) == bytes([255]) * (1 << 21)
    try:
        b"ab" * (1 << 62)
    except (MemoryError, OverflowError):
        pass
    else:
        raise AssertionError("absurd repetition")


run(3000)
print("ok")
