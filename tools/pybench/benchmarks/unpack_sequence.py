"""Tuple and list packing/unpacking, swaps, and starred assignment."""

WORK = 200000


def bench(n):
    a, b, c = 1, 2, 3
    total = 0
    data = [(i, i + 1, i + 2) for i in range(64)]
    lst = [1, 2, 3, 4, 5]
    for i in range(n):
        a, b, c = b, c, a
        x, y, z = data[i & 63]
        first, *rest = lst
        p, q, r, s, t = lst
        (u, v), w = (x, y), z
        total += a + x + first + len(rest) + t + u + w
    return total
