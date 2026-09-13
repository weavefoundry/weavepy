"""Measure allocation and access around small-slot promotion boundaries."""

from pathlib import Path
import runpy

HERE = Path(__file__).resolve().parent
KERNELS = {}
for count in (1, 2, 8, 9, 16):
    names = tuple("slot%d" % i for i in range(count))
    init = "\n".join("        self.%s = value + %d" % (name, i) for i, name in enumerate(names))
    KERNELS["retained_slots_%d" % count] = f'''
class Item:
    __slots__ = {names!r}
    def __init__(self, value):
{init}
def bench():
    items = [Item(i) for i in range(100000)]
    assert sum(item.slot0 for item in items) == 4999950000
    assert sum(item.{names[-1]} for item in items) == {4999950000 + (count - 1) * 100000}
'''
    # Read and update the last populated field: a vector must search its
    # entire active range, so this includes the least favorable position.
    KERNELS["last_slot_access_%d" % count] = f'''
class Item:
    __slots__ = {names!r}
    def __init__(self, value):
{init}
def bench():
    item = Item(0)
    total = 0
    for i in range(100000):
        item.{names[-1]} = i
        total += item.{names[-1]}
    assert total == 4999950000
'''

KERNELS["slot_delete_reinsert"] = '''
class Item:
    __slots__ = tuple("slot%d" % i for i in range(8))
def bench():
    item = Item()
    for i in range(8):
        setattr(item, "slot%d" % i, i)
    total = 0
    for i in range(50000):
        del item.slot3
        item.slot3 = i
        total += item.slot3
    assert total == 1249975000
'''

if __name__ == "__main__":
    runpy.run_path(str(HERE / "probes.py"))["main"](KERNELS)
