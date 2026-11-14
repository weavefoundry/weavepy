"""`obj.__class__` at a hot site is the instance's class now, whatever
answers it (the default, a class's own `__class__`, an assignment), and
sequences holding lists compare as CPython compares them."""

import dataclasses

N = 4000


class Plain:
    pass


class Other:
    pass


class Lying:
    @property
    def __class__(self):
        return Other


class Hooked:
    def __getattribute__(self, name):
        if name == "__class__":
            return Other
        return object.__getattribute__(self, name)


class SubDict(dict):
    pass


p, ly, hk, sd = Plain(), Lying(), Hooked(), SubDict()
for _ in range(N):
    assert p.__class__ is Plain and ly.__class__ is Other and hk.__class__ is Other
    assert sd.__class__ is SubDict and (1).__class__ is int and Plain.__class__ is type
    assert type(ly) is Lying and isinstance(ly, Other) and isinstance(ly, Lying)

# Re-classing an instance under a running loop.
seen = []
q = Plain()
for i in range(N):
    if i == N // 2:
        q.__class__ = Other
    seen.append(q.__class__)
assert seen[0] is Plain and seen[N // 2 - 1] is Plain and seen[N // 2] is Other and seen[-1] is Other


# Sequences that hold lists.
nan = float("nan")
for _ in range(N):
    assert (1, 2, []) == (1, 2, []) and not (1, 2, []) != (1, 2, [])
    assert (1, [2, [3, "x"]], "y") == (1, [2, [3, "x"]], "y")
    assert (1, [2, 3]) != (1, [2, 4]) and (1, [2, 3]) != (1, [2, 3, 4])
    assert (1, [2]) < (1, [3]) and (1, [2, 3]) > (1, [2]) and [(1, [2])] <= [(1, [2])]
    assert [nan] == [nan] and (1, [nan]) != (1, [float("nan")])
    assert (1, [2]) != (1, (2,)) and [[1], [2]] == [[1], [2]]
for _ in range(50):
    try:
        (1, [2]) < (1, (3,))
    except TypeError:
        pass
    else:
        raise AssertionError("a list doesn't order against a tuple")


class Eq:
    calls = 0

    def __init__(self, v):
        self.v = v

    def __eq__(self, other):
        Eq.calls += 1
        return self.v == other.v


for _ in range(1000):
    assert (1, [Eq(1)]) == (1, [Eq(1)]) and (1, [Eq(1)]) != (1, [Eq(2)])
assert Eq.calls == 2000


@dataclasses.dataclass(order=True)
class Rec:
    a: int
    tags: list = dataclasses.field(default_factory=list)


x, y, z = Rec(1, ["t"]), Rec(1, ["t"]), Rec(1, ["u"])
for _ in range(N):
    assert x == y and x != z and x < z and not z < x and x <= y and x != Plain()
print("ok")
