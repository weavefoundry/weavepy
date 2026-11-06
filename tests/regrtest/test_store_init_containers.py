"""An `__init__` that only stores parameters, constants and fresh empty
containers (`self.items = []`, `self.index = {}`) runs as a store-only
initializer: each instance still gets containers of its own, the cycle
collector still finds a cycle closed through one, and a class whose
`__init__` differs keeps its own behavior."""
import gc
import weakref


class Bag:
    def __init__(self, name):
        self.name = name
        self.items = []
        self.index = {}
        self.count = 0


class Node:
    def __init__(self):
        self.children = []


def run(i):
    a, b = Bag("a"), Bag("b")
    assert a.items == [] and a.index == {} and a.count == 0 and a.name == "a"
    assert a.items is not b.items and a.index is not b.index
    a.items.append(i)
    a.index["k"] = i
    assert b.items == [] and b.index == {}
    assert vars(a) == {"name": "a", "items": [i], "index": {"k": i}, "count": 0}


# Past the thresholds, so later rounds take the store-only path.
for i in range(3000):
    run(i)

# A cycle closed through a fresh list is still collected.
n = Node()
n.children.append(n)
r = weakref.ref(n)
del n
gc.collect()
assert r() is None

# Many short-lived instances whose lists hold the instance itself.
for i in range(2000):
    m = Node()
    m.children.append(m)
gc.collect()
print("ok")
