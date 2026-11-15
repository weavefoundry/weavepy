"""`bool`, `slice`, `set` and `frozenset` called at a hot site with plain
arguments build what the general constructors build, and anything that
isn't plain (a subclass with its own truth, an unhashable or NaN element,
a slice of other objects) still gets the general constructor's answer."""

N = 3000
nan = float("nan")


class Loud(list):
    def __bool__(self):
        return False


class Sized:
    def __len__(self):
        return 0


for _ in range(N):
    assert bool([1]) and bool((0,)) and bool({"a": 1}) and bool({0}) and bool(b"x")
    assert bool(frozenset([None])) and bool("s") and bool(1.5) and bool() is False
    assert not bool([]) and not bool(()) and not bool({}) and not bool(set())
    assert not bool(frozenset()) and not bool(b"") and not bool("") and not bool(None)
    assert bool(Loud([1])) is False and bool(Sized()) is False and bool(object()) is True
    assert type(bool([1])) is bool

    s = slice(5)
    assert (s.start, s.stop, s.step) == (None, 5, None) and s == slice(None, 5, None)
    s = slice(1, None)
    assert (s.start, s.stop, s.step) == (1, None, None)
    s = slice(1, 10, 2)
    assert (s.start, s.stop, s.step) == (1, 10, 2) and list(range(20))[s] == [1, 3, 5, 7, 9]
    assert s.indices(6) == (1, 6, 2) and slice("a", 2.5).start == "a" and repr(slice(2)) == "slice(None, 2, None)"

    assert set([1, 2, 2, 3]) == {1, 2, 3} and set((1, 1.0, True)) == {1} and len(set([1, 1.0, True])) == 1
    assert type(next(iter(set([1.0, 1])))) is float
    assert frozenset(["a", "b", "a"]) == frozenset({"a", "b"}) and type(frozenset([1])) is frozenset
    assert set([(1, 2.5), (1, 2.5), None, b"b"]) == {(1, 2.5), None, b"b"}
    assert frozenset(()) == frozenset() and set([]) == set() and len(frozenset([])) == 0
    assert len(set([nan, nan])) == 1 and len(set([float("nan"), float("nan")])) == 2
    made = set([1, 2])
    made.add(3)
    assert made == {1, 2, 3} and hash(frozenset([1, 2])) == hash(frozenset([2, 1]))

for bad in ([[1]], ([],), [{}], [1, [2]]):
    for ctor in (set, frozenset):
        try:
            ctor(bad)
        except TypeError:
            pass
        else:
            raise AssertionError("unhashable element")
for args in ((), (1, 2, 3, 4)):
    try:
        slice(*args)
    except TypeError:
        pass
    else:
        raise AssertionError("slice takes one to three arguments")
print("ok")
