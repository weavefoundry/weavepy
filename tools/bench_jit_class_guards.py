"""Retain compiled readers after their temporary receiver classes die.

Each batch collects the obsolete classes. Payload size is explicit so an
ordinary empty class and a plugin-like class owning data can be measured
separately. Readers remain live and continue to read independent replacement
instances after collection.
"""
import gc
import os
import time

SOURCE = 'def reader(obj, n):\n    total = 0\n    for _ in range(n):\n        total += obj.value\n    return total\n'


def make_reader(payload):
    namespace = {}
    exec(SOURCE, namespace)
    reader = namespace['reader']
    cls = type('Temporary', (), {'payload': b'x' * payload})
    obj = cls()
    obj.value = 7
    assert reader(obj, 10000) == 70000
    return reader


def bench(n):
    payload = int(os.environ.get('WEAVEPY_CLASS_GUARD_PAYLOAD', '0'))
    readers = []
    for i in range(n):
        readers.append(make_reader(payload))
        if i % 16 == 15:
            gc.collect()
    for _ in range(3):
        gc.collect()
    # A fresh class must never satisfy an obsolete class's guard token.
    class Replacement:
        pass
    obj = Replacement()
    obj.padding = -100
    obj.value = 11
    for reader in readers:
        assert reader(obj, 5) == 55
    return readers


if __name__ == '__main__':
    start = time.perf_counter_ns()
    result = bench(int(os.environ.get('WEAVEPY_BENCH_WORK', '256')))
    print('WEAVEPY_BENCH_NS=%d' % (time.perf_counter_ns() - start))
