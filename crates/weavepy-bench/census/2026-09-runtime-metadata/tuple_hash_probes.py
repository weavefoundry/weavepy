"""Measure repeated tuple hashes, key lookups, and hashes without reuse."""

from pathlib import Path
import runpy

HERE = Path(__file__).resolve().parent
KERNELS = {
    "repeated_pair_hash": '''
key = (123, 456)
expected = hash(key)
def bench():
    result = 0
    for _ in range(100000):
        result = hash(key)
    assert result == expected
''',
    "repeated_wide_tuple_hash": '''
key = tuple(range(64))
expected = hash(key)
def bench():
    result = 0
    for _ in range(50000):
        result = hash(key)
    assert result == expected
''',
    "repeated_shared_nested_hash": '''
key = (1, 2, 3, 4)
for _ in range(7):
    key = (key, key)
expected = hash(key)
def bench():
    result = 0
    for _ in range(2000):
        result = hash(key)
    assert result == expected
''',
    "tuple_dictionary_lookups": '''
keys = [tuple(range(i, i + 16)) for i in range(2000)]
data = {key: i for i, key in enumerate(keys)}
def bench():
    total = 0
    for _ in range(20):
        for key in keys:
            total += data[key]
    assert total == 39980000
''',
    "fresh_pair_hashes": '''
def bench():
    values = [hash((i, -i)) for i in range(50000)]
    assert len(values) == 50000
    assert values[123] == hash((123, -123))
''',
    "retained_tuple_keys": '''
def bench():
    values = {(i, -i): i for i in range(100000)}
    assert len(values) == 100000
    assert values[(123, -123)] == 123
    assert sum(values.values()) == 4999950000
''',
    "bounded_cache_tuple_hits": '''
import functools
@functools.lru_cache(maxsize=128)
def cached(first, second):
    return first + second
for i in range(128):
    assert cached(i, -i) == 0
def bench():
    total = 0
    for _ in range(200):
        for i in range(128):
            total += cached(i, -i)
    assert total == 0
    info = cached.cache_info()
    assert info.hits >= 25600 and info.misses == 128
''',
    "unbounded_cache_tuple_misses": '''
import functools
@functools.lru_cache(maxsize=None)
def cached(first, second):
    return first + second
def bench():
    cached.cache_clear()
    total = 0
    for i in range(20000):
        total += cached(i, second=-i)
    assert total == 0
    info = cached.cache_info()
    assert info.misses == 20000 and info.currsize == 20000
''',
}


if __name__ == "__main__":
    runpy.run_path(str(HERE / "probes.py"))["main"](KERNELS)
