"""`min`, `max`, `sorted` and `list.sort` with a builtin key (`key=abs`, `key=len`).

The key's body runs directly for each item; ties keep the first item, a
key that raises propagates its error, `default` applies to an empty
iterable, and Python and `None` keys behave as before. Run it past the
compile threshold.
"""


class Sized:
    def __init__(self, n):
        self.n = n

    def __len__(self):
        return self.n


def run(n):
    for _ in range(n):
        xs = [3, -7, 2, 7, -1]
        assert min(xs, key=abs) == -1 and max(xs, key=abs) == -7
        assert max(["bb", "a", "cc"], key=len) == "bb"
        assert min([2.5, -0.5, True], key=abs) == -0.5
        assert min([], key=abs, default="d") == "d"
        assert max((1, 2), key=None) == 2
        assert min([3, 1], key=lambda v: -v) == 3
        assert sorted([3, -1, 2, -3], key=abs) == [-1, 2, 3, -3]
        assert sorted(["bb", "a", "cc", "d"], key=len, reverse=True) == ["bb", "cc", "a", "d"]
        mixed = [[1, 2], "abc", Sized(0), (1,)]
        assert [len(v) for v in sorted(mixed, key=len)] == [0, 1, 2, 3]
        ws = ["pear", "fig", "kiwi"]
        ws.sort(key=len)
        assert ws == ["fig", "pear", "kiwi"]
        try:
            sorted([1, "x"], key=abs)
        except TypeError:
            pass
        else:
            raise AssertionError("abs of a str in sorted")
        try:
            min([1, "x"], key=abs)
        except TypeError:
            pass
        else:
            raise AssertionError("abs of a str")


run(3000)
print("ok")
