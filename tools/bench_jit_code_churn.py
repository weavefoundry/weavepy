"""Retire compiled module functions while retaining weak metadata watchers.

Each generated module owns a 64 KiB lookup buffer. The workload checks its
results and keeps weak references, as a code-monitoring tool might. Collection
runs three times per module so both tier-cache and cycle-collector releases
settle. Peak RSS includes compilation, module data, and the watchers.
"""
import gc
import os
import weakref

SOURCE = """
def callee(x):
    return x + offset
def caller(n):
    total = 0
    for i in range(n):
        total += callee(i)
    return total
"""


def bench(n):
    total = 0
    watched = []
    for offset in range(n):
        namespace = {'offset': offset, 'lookup': bytearray(65536)}
        exec(SOURCE, namespace)
        total += namespace['caller'](1000)
        watched.extend([weakref.ref(namespace['callee']),
                        weakref.ref(namespace['caller']),
                        weakref.ref(namespace['callee'].__code__),
                        weakref.ref(namespace['caller'].__code__)])
        namespace.pop('caller')
        namespace.pop('callee')
        del namespace
        for _ in range(3):
            gc.collect()
    gc.collect()
    assert total == n * 499500 + 1000 * n * (n - 1) // 2
    return total, watched


if __name__ == '__main__':
    import time
    start = time.perf_counter_ns()
    bench(int(os.environ.get('WEAVEPY_BENCH_WORK', '100')))
    print('WEAVEPY_BENCH_NS=%d' % (time.perf_counter_ns() - start))
