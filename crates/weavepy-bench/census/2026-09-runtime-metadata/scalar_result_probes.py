"""Measure checked scalar results from generic Python calls."""

from pathlib import Path
import runpy

HERE = Path(__file__).resolve().parent
KERNELS = {}
for name, setup, argument, expression, expected in (
    ("integer_callback_sum", "def source(i):\n    return i\n", "source",
     "callback(i)", 2096128),
    ("integer_callback_left", "def source(i):\n    return i\n", "source",
     "callback(i) + 1", 2098176),
    ("keyword_callback_sum", "def source(i, **kwargs):\n    return i + kwargs.get('delta', 0)\n",
     "source", "callback(i, delta=2)", 2100224),
    ("callable_instance_sum", "class Source:\n    def __call__(self, i):\n        return i + 3\nSOURCE = Source()\n",
     "SOURCE", "callback(i)", 2102272),
):
    KERNELS[name] = f'''
{setup}
def collect(n, callback):
    total = 0
    for i in range(n):
        total += {expression}
    return total
def bench(n):
    total = 0
    for _ in range(n):
        total += collect(2048, {argument})
    assert total == n * {expected}
    return total
'''


if __name__ == "__main__":
    runpy.run_path(str(HERE / "jit_probes.py"))["main"](KERNELS)
