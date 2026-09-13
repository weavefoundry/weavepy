"""Measure checked slot reads and writes in warmed native functions."""

from pathlib import Path
import runpy

HERE = Path(__file__).resolve().parent
KERNELS = {}
for count in (1, 8, 16):
    names = tuple("slot%d" % i for i in range(count - 1)) + ("value",)
    KERNELS["native_slots_%d" % count] = f'''
class Item:
    __slots__ = {names!r}
ITEM = Item()
for name in Item.__slots__:
    setattr(ITEM, name, 0)
def calculate(item, n):
    total = 0
    for i in range(n):
        item.value = i
        total += item.value
    return total
def bench(n):
    total = 0
    for _ in range(n):
        total += calculate(ITEM, 2048)
    assert total == n * 2096128
    return total
'''

KERNELS["native_alternating_slot_orders"] = '''
class Item:
    __slots__ = ("a", "b", "c", "d", "e", "f", "g", "value")
FIRST = Item()
SECOND = Item()
for name in Item.__slots__:
    setattr(FIRST, name, 0)
for name in reversed(Item.__slots__):
    setattr(SECOND, name, 0)
def calculate(item, n):
    total = 0
    for i in range(n):
        item.value = i
        total += item.value
    return total
def bench(n):
    total = 0
    for _ in range(n):
        total += calculate(FIRST, 2048)
        total += calculate(SECOND, 2048)
    assert total == n * 4192256
    return total
'''

if __name__ == "__main__":
    runpy.run_path(str(HERE / "jit_probes.py"))["main"](KERNELS)
