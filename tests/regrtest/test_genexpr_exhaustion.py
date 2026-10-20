"""Generator expressions run to exhaustion by compiled code, whose `.0`
iterator the loop shares with the generator's local: ranges, strings,
bytes, lists (of plain scalars, and of objects whose finalizers must run
when the generator lets go of them), sets, and tuples. Each runs enough
times for the compiled body to take it."""
import gc

ROUNDS = 500

log = []


class Fin:
    def __init__(self, i):
        self.i = i

    def __del__(self):
        log.append(self.i)


def build(xs):
    return tuple(x for x in xs)


for r in range(ROUNDS):
    n = r % 7
    assert build(range(n)) == tuple(range(n))
    assert build("abc"[: n % 4]) == tuple("abc"[: n % 4])
    assert build(b"xyz"[: n % 4]) == tuple(b"xyz"[: n % 4])
    assert build([1, 2.5, None, "s", True][:n]) == (1, 2.5, None, "s", True)[:n]
    assert sorted(build({1, 2, 3, n})) == sorted({1, 2, 3, n})
    assert build((n, n + 1)) == (n, n + 1)
    assert set(c for c in "hello") == {"h", "e", "l", "o"}
    assert sum(i for i in range(n)) == n * (n - 1) // 2
    assert all(c.isalpha() for c in "abc")
    assert not any(i < 0 for i in range(n))

    # The objects of a temporary list a generator expression walked die
    # as the finished generator lets go of the list's iterator.
    log.clear()
    g = (f.i for f in [Fin(1), Fin(2)])
    assert list(g) == [1, 2]
    assert sorted(log) == [1, 2], log
    del g

    # A set shrinking mid-walk still raises.
    s = {1, 2, 3}
    try:
        for v in (x for x in s):
            s.discard(3)
            s.discard(2)
    except RuntimeError:
        pass
    else:
        raise AssertionError("set size change not detected")
print("genexpr exhaustion ok")
