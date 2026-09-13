"""Measure slot index hits, alternate orders, and promotion boundaries."""

from pathlib import Path
import runpy

HERE = Path(__file__).resolve().parent
KERNELS = dict(runpy.run_path(str(HERE / "small_slot_probes.py"))["KERNELS"])
for index in (0, 3):
    KERNELS["slot_access_8_index_%d" % index] = f'''
class Item:
    __slots__ = tuple("slot%d" % i for i in range(8))
def bench():
    item = Item()
    for i in range(8):
        setattr(item, "slot%d" % i, i)
    total = 0
    for i in range(100000):
        item.slot{index} = i
        total += item.slot{index}
    assert total == 4999950000
'''

KERNELS["alternating_slot_orders"] = '''
class Item:
    __slots__ = tuple("slot%d" % i for i in range(8))
def update(item, value):
    item.slot7 = value
    return item.slot7
def bench():
    forward = Item()
    reverse = Item()
    for i in range(8):
        setattr(forward, "slot%d" % i, i)
        setattr(reverse, "slot%d" % (7-i), i)
    total = 0
    for i in range(50000):
        total += update(forward, i)
        total += update(reverse, i)
    assert total == 2499950000
'''

if __name__ == "__main__":
    runpy.run_path(str(HERE / "probes.py"))["main"](KERNELS)
