"""Measure saved bound calls, including setup and result validation."""
import os
import time

KIND = os.environ.get('WEAVEPY_BOUND_CALL_KIND', 'method')
if KIND not in ('method', 'keyword', 'classmethod', 'builtin', 'iterator', 'generator'):
    raise ValueError('unknown WEAVEPY_BOUND_CALL_KIND: ' + KIND)


class Receiver:
    value = 7

    def read(self, *, extra=0):
        return self.value + extra

    @classmethod
    def class_read(cls):
        return cls.value


def sum_calls(callback, n):
    total = 0
    for _ in range(n):
        total += callback()
    return total


def sum_keyword_calls(callback, n):
    total = 0
    for _ in range(n):
        total += callback(extra=2)
    return total


def bench(n):
    if n < 0:
        raise ValueError('work must be nonnegative')
    iterator = None
    if KIND in ('method', 'keyword'):
        receiver = Receiver()
        callback = receiver.read
        expected = (9 if KIND == 'keyword' else 7) * n
    elif KIND == 'classmethod':
        callback = Receiver.class_read
        expected = 7 * n
    elif KIND == 'builtin':
        receiver = [2, 3, 5]
        callback = receiver.__len__
        expected = 3 * n
    else:
        iterator = iter(range(n)) if KIND == 'iterator' else (i for i in range(n))
        callback = iterator.__next__
        expected = n * (n - 1) // 2
    result = (sum_keyword_calls(callback, n) if KIND == 'keyword'
              else sum_calls(callback, n))
    assert result == expected, (result, expected)
    if iterator is not None:
        sentinel = object()
        assert next(iterator, sentinel) is sentinel
    return result


if __name__ == '__main__':
    start = time.perf_counter_ns()
    bench(int(os.environ.get('WEAVEPY_BENCH_WORK', '200000')))
    print('WEAVEPY_BENCH_NS=%d' % (time.perf_counter_ns() - start))
