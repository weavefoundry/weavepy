"""Sequential workers compile private numeric functions, then exit.

Each worker defines 20 distinct numeric functions and runs a 2,000-iteration
loop in each. Clearing its private namespace releases Python references;
joining the worker exercises the lifetime of its thread-local native code.
The result check catches missing workers and incorrect compiled results.
Peak RSS includes the interpreter, compilation, thread stacks, and code.
"""
import os
import threading
import time

SOURCE = '\n'.join(
    'def kernel%d(n):\n    total = %d\n    for i in range(n):\n        total += i\n    return total\n' % (i, i)
    for i in range(20)
)


def work(results):
    namespace = {}
    exec(SOURCE, namespace)
    total = 0
    for i in range(20):
        total += namespace['kernel%d' % i](2000)
    namespace.clear()
    results.append(total)


def bench(n):
    results = []
    for _ in range(n):
        worker = threading.Thread(target=work, args=(results,))
        worker.start()
        worker.join()
    assert results == [20 * 1999000 + 190] * n
    return results


if __name__ == '__main__':
    start = time.perf_counter_ns()
    bench(int(os.environ.get('WEAVEPY_BENCH_WORK', '100')))
    print('WEAVEPY_BENCH_NS=%d' % (time.perf_counter_ns() - start))
