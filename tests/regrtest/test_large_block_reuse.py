"""Large buffers freed and allocated again, and repetition of sequences.

A freed block too large for the allocator's arenas is kept a while for an
allocation of about its size, so it must come back whole: a zeroed
allocation (`bytearray(n)`, `bytes(n)`) after a filled one of the same
size reads all zeros, and nothing written to a kept block shows through.
`[x] * n` and `(x,) * n` copy values without a reference count bitwise
and alias anything else, as CPython does. Run it past the compile
threshold, with sizes on both sides of the in-line limits.
"""

N = 3 * 1024 * 1024


def large_rounds():
    for i in range(6):
        filled = bytes([i + 1]) * N
        assert filled.count(i + 1) == N
        del filled
        z = bytearray(N)
        assert z.count(0) == N and len(z) == N
        z[::4096] = b"\x07" * len(z[::4096])
        del z
        assert bytes(N).count(0) == N
        lst = [i] * (N // 8)
        assert lst[-1] == i and len(lst) == N // 8
        del lst
        assert [None] * (N // 8) == [None] * (N // 8)


def repeats(n):
    for i in range(n):
        k = i % 7
        assert [0, 1.5, True, None] * k == [0, 1.5, True, None] * k
        assert len([i] * 5000) == 5000 and ([i] * 5000)[4999] == i
        t = (i, -2.0) * k
        assert len(t) == 2 * k and (k == 0 or t[-2:] == (i, -2.0))
        shared = [[]] * 3
        shared[0].append(i)
        assert shared[2] == [i]
        s = ["ab", i] * 3
        assert s == ["ab", i, "ab", i, "ab", i] and s[0] is s[4]
        big = 10**30
        assert ([big] * 2)[1] is big
        assert [1] * -3 == [] and (1,) * 0 == ()


large_rounds()
repeats(3000)
large_rounds()
print("ok")
