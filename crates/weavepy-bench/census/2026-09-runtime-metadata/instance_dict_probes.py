"""Measure cold, exported, populated, native, and slotted instance dictionaries."""
from pathlib import Path
import runpy

HERE = Path(__file__).resolve().parent
KERNELS = {}

for label, declaration, extra in [
    ('cold_plain', 'class Item:\n    pass\n', ''),
    ('cold_empty_slots', 'class Item:\n    __slots__ = ()\n', ''),
    ('exported_empty_dict', 'class Item:\n    pass\n',
     '    dictionaries = [vars(item) for item in items]\n'
     '    assert all(not namespace for namespace in dictionaries)\n'),
    ('populated_dict', 'class Item:\n    pass\n',
     '    for i, item in enumerate(items):\n'
     '        item.value = i\n'
     '    assert sum(item.value for item in items) == 4999950000\n'),
    ('cold_native_int', 'class Item(int):\n    pass\n', ''),
    ('cold_native_list', 'class Item(list):\n    pass\n', ''),
]:
    KERNELS['retained_' + label] = declaration + '''
def bench():
    items = [Item() for _ in range(100000)]
    assert len(items) == 100000
''' + extra

for label, declaration in [
    ('plain', 'class Item:\n    pass\n'),
    ('empty_slots', 'class Item:\n    __slots__ = ()\n'),
]:
    KERNELS['churn_' + label] = declaration + '''
def bench():
    for _ in range(100000):
        item = Item()
        del item
'''

KERNELS['class_attribute_cold_dict'] = '''
class Item:
    value = 7
items = [Item() for _ in range(1000)]
def bench():
    total = 0
    for _ in range(100):
        for item in items:
            total += item.value
    assert total == 700000
'''
KERNELS['method_cold_dict'] = '''
class Item:
    def value(self):
        return 7
items = [Item() for _ in range(1000)]
def bench():
    total = 0
    for _ in range(100):
        for item in items:
            total += item.value()
    assert total == 700000
'''

if __name__ == '__main__':
    runpy.run_path(str(HERE / 'probes.py'))['main'](KERNELS)
