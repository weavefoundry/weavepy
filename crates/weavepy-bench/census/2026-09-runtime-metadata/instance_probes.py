"""Measure compact caches and slot storage against the preceding runtime."""

from pathlib import Path
import runpy

HERE = Path(__file__).resolve().parent
KERNELS = {
    "empty_instances": '''
class Item:
    pass
def bench():
    items = [Item() for _ in range(100000)]
    assert len(items) == 100000 and items[0] is not items[-1]
''',
    "one_slot_instances": '''
class Item:
    __slots__ = ("value",)
    def __init__(self, value):
        self.value = value
def bench():
    items = [Item(i) for i in range(100000)]
    assert sum(item.value for item in items) == 4999950000
''',
    "two_slot_instances": '''
class Item:
    __slots__ = ("first", "second")
    def __init__(self, value):
        self.first = value
        self.second = -value
def bench():
    items = [Item(i) for i in range(100000)]
    assert sum(item.first for item in items) == 4999950000
    assert sum(item.second for item in items) == -4999950000
''',
    "many_slot_instances": '''
class Item:
    __slots__ = ("a", "b", "c", "d", "e", "f", "g", "h")
    def __init__(self, value):
        self.a = value
        self.b = value + 1
        self.c = value + 2
        self.d = value + 3
        self.e = value + 4
        self.f = value + 5
        self.g = value + 6
        self.h = value + 7
def bench():
    items = [Item(i) for i in range(100000)]
    assert sum(item.a for item in items) == 4999950000
    assert sum(item.h for item in items) == 5000650000
''',
    "singleton_frozensets": '''
def bench():
    items = [frozenset((i,)) for i in range(100000)]
    assert len(items) == 100000
    assert items[0] is not items[-1]
    assert all(len(item) == 1 for item in items)
    assert sum(next(iter(item)) for item in items) == 4999950000
''',
    "cached_frozenset_hashes": '''
def bench():
    items = [frozenset((i,)) for i in range(50000)]
    hashes = [hash(item) for item in items]
    for _ in range(3):
        result = [hash(item) for item in items]
    assert result == hashes
    assert len(set(hashes)) > 49000
''',
    "cached_weakref_hashes": '''
import weakref
class Item:
    __slots__ = ("value", "__weakref__")
    def __init__(self, value):
        self.value = value
    def __hash__(self):
        return self.value
def bench():
    items = [Item(i) for i in range(20000)]
    refs = [weakref.ref(item) for item in items]
    assert sum(hash(ref) for ref in refs) == 199990000
    assert sum(hash(ref) for ref in refs) == 199990000
''',
}

for count in (1, 2, 8):
    names = tuple("slot%d" % i for i in range(count))
    init = "\n".join("        self.%s = 0" % name for name in names)
    # Prepopulate every declared slot so reads exercise both inline and
    # promoted storage, while the repeated operations stay comparable.
    KERNELS["slot_access_%d" % count] = f'''
class Item:
    __slots__ = {names!r}
    def __init__(self):
{init}
def bench():
    item = Item()
    total = 0
    for i in range(100000):
        item.slot0 = i
        total += item.slot0
    assert total == 4999950000
'''


if __name__ == "__main__":
    runpy.run_path(str(HERE / "probes.py"))["main"](KERNELS)
