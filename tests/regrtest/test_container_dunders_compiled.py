"""Container shapes compiled code now runs itself keep CPython's results
and errors: `dict.fromkeys` and `list(d)` (the dedup idiom), tuple keys of
`int` subclasses and `None`, a Python `__len__` as an instance's truth,
`obj[a:b]` through a Python `__getitem__`, and `x[a:b:c]`."""


class Op(int):
    pass


A, B = Op(1), Op(2)


class Seq:
    def __init__(self, data):
        self.data = data

    def __len__(self):
        return len(self.data)

    def __getitem__(self, i):
        if isinstance(i, slice):
            return Seq(self.data[i])
        return self.data[i]


class Negative:
    def __len__(self):
        return -1


class NotInt:
    def __len__(self):
        return "x"


class BoolWins:
    def __bool__(self):
        return False

    def __len__(self):
        return 1


class Refuses:
    def __getitem__(self, i):
        raise LookupError(i)


class D(dict):
    pass


def check(i):
    # The dedup idiom: first occurrences, in order; equal keys keep the first.
    items = [(A, 97), (B, (48, 57)), (1, 97), (None, None), (A, 97)]
    assert list(dict.fromkeys(items)) == [(A, 97), (B, (48, 57)), (None, None)]
    keys = list(dict.fromkeys([1, 1.0, True, "a"]))
    assert keys == [1, "a"] and type(keys[0]) is int
    assert dict.fromkeys(("x", "y"), i) == {"x": i, "y": i}
    assert type(D.fromkeys([1])) is D
    try:
        dict.fromkeys([[1]])
    except TypeError:
        pass
    else:
        raise AssertionError("an unhashable key didn't raise")
    assert list({3: 1, 1: 2}) == [3, 1]
    assert {(A, (B, None)): 1}[(1, (2, None))] == 1

    s = Seq([1, 2, 3, 4])
    assert bool(s) and not Seq([])
    assert (1 if s else 0) == 1 and (1 if Seq([]) else 0) == 0
    assert not BoolWins()
    for bad, exc in ((Negative(), ValueError), (NotInt(), TypeError)):
        try:
            if bad:
                pass
        except exc:
            pass
        else:
            raise AssertionError(f"{type(bad).__name__} didn't raise")

    assert s[-1:].data == [4] and s[1:3].data == [2, 3] and s[:].data == [1, 2, 3, 4]
    try:
        Refuses()[1:2]
    except LookupError as e:
        assert e.args == (slice(1, 2),), e.args
    else:
        raise AssertionError("the slice didn't raise")
    assert list(range(4)[::-1]) == [3, 2, 1, 0] and [5, 6, 7, 8][1:3:1] == [6, 7]
    assert "abcdef"[::2] == "ace" and s[::2].data == [1, 3]


# Past the frame JIT's threshold, so later rounds run compiled code.
for i in range(3000):
    check(i)
print("ok")
