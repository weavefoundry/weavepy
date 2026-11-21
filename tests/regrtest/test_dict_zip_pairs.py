"""`dict(zip(ks, vs))` over sources that run Python code.

The pairs come straight from the two sources in the order the zip steps
them; an item the first source gave is dropped when the second has none;
errors from either source or from an unhashable key propagate; strict
zips, other arities and iterables keep the general path.
"""

log = []


def gen(xs, tag):
    for x in xs:
        log.append((tag, x))
        yield x


def boom(n):
    yield from range(n)
    raise RuntimeError("boom")


def run(n):
    for _ in range(n):
        log.clear()
        assert dict(zip("abc", gen(range(3), "v"))) == {"a": 0, "b": 1, "c": 2}
        log.clear()
        d = dict(zip(gen("abcd", "k"), gen(range(2), "v")))
        assert d == {"a": 0, "b": 1}
        assert log == [("k", "a"), ("v", 0), ("k", "b"), ("v", 1), ("k", "c")], log
        log.clear()
        assert dict(zip(gen("ab", "k"), gen(range(5), "v"))) == {"a": 0, "b": 1}
        assert log == [("k", "a"), ("v", 0), ("k", "b"), ("v", 1)], log
        assert dict(zip("aa", range(2))) == {"a": 1}
        k = iter("xyz")
        assert dict(zip(k, gen(range(1), "v"))) == {"x": 0} and list(k) == ["z"]
        try:
            dict(zip("abcd", boom(2)))
        except RuntimeError:
            pass
        else:
            raise AssertionError("source error")
        try:
            dict(zip(gen([[1]], "k"), "a"))
        except TypeError:
            pass
        else:
            raise AssertionError("unhashable key")
        try:
            dict(zip("ab", gen(range(3), "v"), strict=True))
        except ValueError:
            pass
        else:
            raise AssertionError("strict")
        assert dict(zip(gen(range(2), "k"), "ab", strict=True)) == {0: "a", 1: "b"}
        try:
            dict(zip(gen([(1, 2)], "p")))
        except ValueError:
            pass
        else:
            raise AssertionError("one-item tuples")
        assert dict(x for x in [(1, 2)]) == {1: 2}


run(400)
print("ok")
