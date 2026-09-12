"""Measure valid dynamic, open-ended, and folded native list/string slices."""
from pathlib import Path
import runpy

HERE = Path(__file__).resolve().parent
KERNELS = {}
for kind, data in [('list', 'list(range(64))'), ('str', "'abcd' * 16")]:
    for label, expression, expected in [
        ('dynamic', 'data[1:stop:None]', 2880),
        ('open', 'data[stop - 16::None]', 7232),
        ('constant', 'data[1:24]', 2944),
    ]:
        KERNELS[kind + '_' + label] = f'''
DATA = {data}
def calculate(data):
    total = 0
    for i in range(128):
        stop = (i & 15) + 16
        total += len({expression})
    return total
def bench(n):
    total = 0
    for _ in range(n):
        total += calculate(DATA)
    assert total == n * {expected}
    return total
'''

for kind, data in [('list', 'list(range(64))'), ('str', "'abcd' * 16")]:
    KERNELS[kind + '_parameter'] = f'''
DATA = {data}
def calculate(data, stop):
    total = 0
    for i in range(128):
        total += len(data[1:stop:None])
    return total
def bench(n):
    total = 0
    for _ in range(n):
        total += calculate(DATA, 24)
    assert total == n * 2944
    return total
'''

if __name__ == '__main__':
    runpy.run_path(str(HERE / 'jit_probes.py'))['main'](KERNELS)
