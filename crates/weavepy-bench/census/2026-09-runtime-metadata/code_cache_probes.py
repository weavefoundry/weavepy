"""Measure code caches, unexecuted code controls, and class-version churn."""
from pathlib import Path
import runpy

HERE = Path(__file__).resolve().parent
KERNELS = {}
for warm in (False, True):
    KERNELS['retained_warm_code' if warm else 'retained_cold_code'] = f'''
SOURCE = "def calculate(x):\\n" + "    x = x + 1\\n" * 64 + "    return x\\n"
def bench():
    functions = []
    for i in range(500):
        namespace = {{}}
        exec(compile(SOURCE, "retained_code_%d.py" % i, "exec"), namespace)
        function = namespace["calculate"]
        functions.append(function)
        if {warm!r}:
            assert function(i) == i + 64
    assert len(functions) == 500
    assert all(callable(function) for function in functions)
'''

KERNELS['code_compile_churn'] = '''
SOURCE = "def calculate(x):\\n" + "    x = x + 1\\n" * 64 + "    return x\\n"
def bench():
    total = 0
    for i in range(500):
        namespace = {}
        exec(compile(SOURCE, "temporary_code_%d.py" % i, "exec"), namespace)
        total += namespace["calculate"](i)
        namespace.clear()
    assert total == 156750
'''
KERNELS['class_version_churn'] = '''
class Item:
    pass
def bench():
    total = 0
    for i in range(100000):
        Item.value = i
        total += Item.value
    assert total == 4999950000
'''
KERNELS['type_creation'] = runpy.run_path(str(HERE / 'probes.py'))['KERNELS']['type_creation']

if __name__ == '__main__':
    runpy.run_path(str(HERE / 'probes.py'))['main'](KERNELS)
