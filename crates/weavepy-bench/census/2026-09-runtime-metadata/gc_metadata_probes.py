"""Measure retained containers and collector work with paired processes."""
from pathlib import Path
import runpy

HERE = Path(__file__).resolve().parent
KERNELS = {}
for name, expression in [
    ('lists', '[]'), ('dicts', '{}'), ('sets', 'set()'), ('tuples', '([], )'),
]:
    for disabled in [False, True]:
        suffix = '_gc_disabled' if disabled else ''
        setup = 'gc.disable()' if disabled else ''
        KERNELS['retained_' + name + suffix] = '''
import gc
''' + setup + '''
def bench():
    items = [''' + expression + ''' for _ in range(100000)]
    assert len(items) == 100000
    assert items[0] is not items[-1]
'''

KERNELS['rooted_full_scans'] = '''
import gc
gc.disable()
items = [[None] for _ in range(50000)]
for i, item in enumerate(items):
    item[0] = items[(i + 1) % len(items)]
del item
def bench():
    for _ in range(5):
        gc.collect(2)
    assert len(items) == 50000
    assert items[-1][0] is items[0]
'''
KERNELS['unreachable_cycle_scans'] = '''
import gc
import weakref
gc.disable()
class Node:
    __slots__ = ('other', '__weakref__')
def cycles():
    nodes = [Node() for _ in range(10000)]
    for i, node in enumerate(nodes):
        node.other = nodes[(i + 1) % len(nodes)]
    return [weakref.ref(node) for node in nodes]
def bench():
    for _ in range(5):
        references = cycles()
        gc.collect(2)
        assert all(reference() is None for reference in references)
'''
KERNELS['freeze_unfreeze_scans'] = '''
import gc
gc.disable()
items = [[None] for _ in range(50000)]
for i, item in enumerate(items):
    item[0] = items[(i + 1) % len(items)]
del item
def bench():
    for _ in range(5):
        gc.freeze()
        assert gc.get_freeze_count() >= 50000
        gc.collect(2)
        gc.unfreeze()
        assert gc.get_freeze_count() == 0
    assert items[-1][0] is items[0]
'''

if __name__ == '__main__':
    runpy.run_path(str(HERE / 'probes.py'))['main'](KERNELS)
