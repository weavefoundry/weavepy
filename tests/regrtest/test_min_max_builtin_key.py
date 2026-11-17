"""`min` and `max` with a builtin key (`key=abs`, `key=len`).

The key's body runs directly for each item; ties keep the first item, a
key that raises propagates its error, `default` applies to an empty
iterable, and Python and `None` keys behave as before. Run it past the
compile threshold.
"""


def run(n):
    for _ in range(n):
        xs = [3, -7, 2, 7, -1]
        assert min(xs, key=abs) == -1 and max(xs, key=abs) == -7
        assert max(["bb", "a", "cc"], key=len) == "bb"
        assert min([2.5, -0.5, True], key=abs) == -0.5
        assert min([], key=abs, default="d") == "d"
        assert max((1, 2), key=None) == 2
        assert min([3, 1], key=lambda v: -v) == 3
        try:
            min([1, "x"], key=abs)
        except TypeError:
            pass
        else:
            raise AssertionError("abs of a str")


run(3000)
print("ok")
