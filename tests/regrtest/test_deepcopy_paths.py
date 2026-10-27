"""The operations `copy.deepcopy` leans on keep Python's semantics in
compiled code: `type(x)`, a call's `*args` spread (`LIST_EXTEND`), and
deep copies of shared, cyclic and reconstructed objects.
"""

import copy
from dataclasses import dataclass, field


def spread(f, args, n):
    out = None
    for _ in range(n):
        out = f(*args)
    return out


def three(a, b, c):
    return (a, b, c)


class Iterable3:
    def __iter__(self):
        return iter((7, 8, 9))


N = 3000
assert spread(three, (1, 2, 3), N) == (1, 2, 3)
assert spread(three, [4, 5, 6], N) == (4, 5, 6)
assert spread(three, "abc", N) == ("a", "b", "c")
assert spread(three, {"x": 1, "y": 2, "z": 3}, N) == ("x", "y", "z")
assert spread(three, Iterable3(), N) == (7, 8, 9)
assert spread(three, (i for i in range(3)), 1) == (0, 1, 2)
try:
    spread(three, 5, 1)
except TypeError as e:
    assert "after * must be an iterable, not int" in str(e), e
else:
    raise AssertionError("a non-iterable spread")


def display(xs, ys, n):
    out = None
    for _ in range(n):
        out = [*xs, *ys, *xs]
    return out


xs = [1, 2]
assert display(xs, (3,), N) == [1, 2, 3, 1, 2]
assert display(xs, xs, N) == [1, 2, 1, 2, 1, 2]


class Fake:
    @property
    def __class__(self):
        return int


def types(values, n):
    out = None
    for _ in range(n):
        out = [type(v) for v in values]
    return out


fake = Fake()
values = [1, 1.5, "s", None, (), [], {}, type, Fake, fake, True]
assert types(values, N) == [int, float, str, type(None), tuple, list, dict, type, type, Fake, bool]
assert fake.__class__ is int and type(fake) is Fake


def shadowed(n):
    type = len  # noqa: A001
    out = None
    for _ in range(n):
        out = type([1, 2, 3])
    return out


assert shadowed(N) == 3


@dataclass
class Item:
    name: str
    price: float
    tags: list = field(default_factory=list)


class Node:
    def __init__(self, value, children):
        self.value = value
        self.children = children


def check(n):
    shared = [1, 2]
    tree = Node(0, [Node(i, [shared, shared]) for i in range(3)])
    tree.children.append(tree)
    src = {"tree": tree, "items": [Item("a", 1.5, ["x", shared])], "t": (1, shared), "s": {1, 2}}
    for _ in range(n):
        dup = copy.deepcopy(src)
    t = dup["tree"]
    assert t is not tree and t.children[-1] is t
    kids = t.children[:3]
    assert [k.value for k in kids] == [0, 1, 2]
    assert kids[0].children[0] is kids[1].children[1] is dup["t"][1] is dup["items"][0].tags[1]
    assert dup["t"][1] is not shared and dup["t"][1] == [1, 2]
    assert dup["items"][0] == Item("a", 1.5, ["x", [1, 2]])
    assert dup["s"] == {1, 2} and dup["s"] is not src["s"]


check(300)
print("ok")
