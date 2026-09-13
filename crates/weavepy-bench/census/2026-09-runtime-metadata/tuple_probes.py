"""Measure small tuple construction and native tuple-producing operations."""

from pathlib import Path
import runpy

HERE = Path(__file__).resolve().parent
KERNELS = {
    "tuple_singletons": '''
def bench():
    items = [(i,) for i in range(100000)]
    assert len(items) == 100000
    assert sum(item[0] for item in items) == 4999950000
''',
    "tuple_pairs": '''
def bench():
    items = [(i, -i) for i in range(100000)]
    assert len(items) == 100000
    assert sum(item[0] for item in items) == 4999950000
    assert sum(item[1] for item in items) == -4999950000
''',
    "tuple_triples": '''
def bench():
    items = [(i, -i, i + 1) for i in range(100000)]
    assert len(items) == 100000
    assert sum(item[0] for item in items) == 4999950000
    assert sum(item[2] for item in items) == 5000050000
''',
    "tuple_eight_items": '''
def bench():
    items = [(i, i, i, i, i, i, i, i) for i in range(100000)]
    assert len(items) == 100000
    assert sum(item[7] for item in items) == 4999950000
''',
    "tuple_pair_churn": '''
def bench():
    total = 0
    for i in range(100000):
        pair = (i, i + 1)
        total += pair[0] + pair[1]
    assert total == 10000000000
''',
    "dictionary_items": '''
data = {i: i + 1 for i in range(100000)}
def bench():
    for _ in range(5):
        items = list(data.items())
    assert len(items) == 100000
    assert sum(item[0] for item in items) == 4999950000
    assert sum(item[1] for item in items) == 5000050000
''',
    "enumeration_items": '''
data = tuple(range(100000))
def bench():
    for _ in range(5):
        items = list(enumerate(data))
    assert len(items) == 100000
    assert sum(item[0] for item in items) == 4999950000
    assert sum(item[1] for item in items) == 4999950000
''',
    "string_partition": '''
def bench():
    items = ["left:right".partition(":") for _ in range(100000)]
    assert len(items) == 100000
    assert all(item == ("left", ":", "right") for item in items)
''',
}

if __name__ == "__main__":
    runpy.run_path(str(HERE / "probes.py"))["main"](KERNELS)
