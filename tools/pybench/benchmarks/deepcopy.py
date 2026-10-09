"""copy.deepcopy of nested containers, instances, and dataclasses."""

import copy
from dataclasses import dataclass, field

WORK = 600


@dataclass
class Item:
    name: str
    price: float
    tags: list = field(default_factory=list)


class Node:
    def __init__(self, value, children):
        self.value = value
        self.children = children


def make():
    tree = Node(0, [Node(i, [Node(j, []) for j in range(3)]) for i in range(4)])
    return {
        "items": [Item("item%d" % i, i * 1.5, ["a", "b", i]) for i in range(10)],
        "matrix": [[i * j for j in range(8)] for i in range(8)],
        "meta": {"name": "inventory", "version": (1, 2, 3), "flags": {"x", "y"}},
        "tree": tree,
        "text": "x" * 100,
    }


def bench(n):
    src = make()
    total = 0
    for _ in range(n):
        dup = copy.deepcopy(src)
        total += len(dup["items"]) + dup["matrix"][7][7] + len(dup["tree"].children)
        total += len(copy.copy(src["items"]))
    return total
