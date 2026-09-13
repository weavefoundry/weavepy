"""Measure exact-name attribute-cache lookups and cache population."""

from pathlib import Path
import runpy

HERE = Path(__file__).resolve().parent
KERNELS = {}
for name, spelling, missing in (
    ('cached_short_attribute', 'value', False),
    ('cached_missing_attribute', 'missing', True),
    ('cached_long_attribute', 'a_long_attribute_name_requiring_exact_comparison', False),
):
    namespace = '{}' if missing else repr({spelling: 42})
    KERNELS[name] = f'''
Base = type('Base', (), {namespace})
class Child(Base):
    pass
OBJ = Child()

def collect(n):
    total = 0
    for _ in range(n):
        total += getattr(OBJ, {spelling!r}, 42)
    return total

def bench(n):
    total = 0
    for _ in range(n):
        total += collect(1024)
    assert total == n * 43008
    return total
'''
KERNELS['many_class_namespaces'] = '''
OBJECTS = [type('Item', (), {'value': 42})() for _ in range(512)]

def collect(n):
    total = 0
    for i in range(n):
        total += getattr(OBJECTS[i & 511], 'value')
    return total

def bench(n):
    total = 0
    for _ in range(n):
        total += collect(1024)
    assert total == n * 43008
    return total
'''

if __name__ == '__main__':
    runpy.run_path(str(HERE / 'jit_probes.py'))['main'](KERNELS)
