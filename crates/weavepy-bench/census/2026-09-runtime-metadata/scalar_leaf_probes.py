"""Measure scalar native arguments, defaults, and arithmetic fallbacks."""

from pathlib import Path
import runpy

HERE = Path(__file__).resolve().parent
KERNELS = {}
for name, setup, expression, expected in (
    ('scalar_pair', 'def source(a, b):\n    return a + b\n', 'callback(i, 1)', 2098176),
    ('trailing_scalar_defaults', 'def source(a, b=3, c=4):\n    return a + b + c\n',
     'callback(i)', 2110464),
    ('float_scalar_pair', 'def source(a, b):\n    return (a + 0.0) / (b + 0.0)\n',
     'callback(i + 0.0, 2.0)', 1048064.0),
    ('keyword_gap_control', 'def source(a, b=3, c=4):\n    return a + b + c\n',
     'callback(i, c=5)', 2112512),
    ('overflow_scalar_control', 'def source(a, b):\n    return a + b\n',
     'callback(9223372036854775807, 1)', 2048 * 9223372036854775808),
):
    KERNELS[name] = f'''
{setup}
def collect(n, callback):
    total = {0.0 if name == 'float_scalar_pair' else 0}
    for i in range(n):
        total += {expression}
    return total

def bench(n):
    total = {0.0 if name == 'float_scalar_pair' else 0}
    for _ in range(n):
        total += collect(2048, source)
    assert total == n * {expected}
    return total
'''


if __name__ == '__main__':
    runpy.run_path(str(HERE / 'jit_probes.py'))['main'](KERNELS)
