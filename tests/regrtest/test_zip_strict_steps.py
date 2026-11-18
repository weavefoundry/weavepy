"""`zip(..., strict=True)` over plain sequences, stepped without the
general path while every source has an item.

Equal lengths give the pairs; a shorter or longer source raises the
`ValueError` CPython raises, after the items before it, whether the zip is
collected, looped over, or stepped with `next`. Run it past the compile
threshold.
"""


def collect(a, b, c=None):
    srcs = (a, b) if c is None else (a, b, c)
    try:
        return list(zip(*srcs, strict=True))
    except ValueError as e:
        return str(e)


def looped(a, b):
    out = []
    try:
        for x, y in zip(a, b, strict=True):
            out.append(x + y)
    except ValueError as e:
        out.append(str(e))
    return out


def stepped(a, b):
    it = zip(a, b, strict=True)
    out = [next(it) for _ in range(min(len(a), len(b)))]
    try:
        next(it)
    except StopIteration:
        out.append("stop")
    except ValueError as e:
        out.append(str(e))
    return out


def run(n):
    for i in range(n):
        k = i % 4
        a, b = list(range(k)), tuple(range(10, 10 + k))
        assert collect(a, b) == list(zip(a, b))
        assert collect(a, b + (1,)) == "zip() argument 2 is longer than argument 1"
        assert collect(a + [1], b) == "zip() argument 2 is shorter than argument 1"
        assert collect(a, b, a + [0]) == "zip() argument 3 is longer than arguments 1-2"
        assert looped(a, b) == [x + y for x, y in zip(a, b)]
        assert looped(a, b + (1,))[-1] == "zip() argument 2 is longer than argument 1"
        assert stepped(a, b)[-1] == "stop"
        assert stepped(a + [5], b)[-1] == "zip() argument 2 is shorter than argument 1"
        assert list(zip(strict=True)) == []
        gen = (x for x in range(k))
        assert collect(gen, b) == list(zip(range(k), b))


run(3000)
print("ok")
