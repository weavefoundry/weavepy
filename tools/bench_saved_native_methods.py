"""Measure saved native methods with setup, result release, and checks timed."""
import os
import time

KIND = os.environ.get('WEAVEPY_SAVED_NATIVE_KIND', 'len')
KINDS = ('len', 'copy', 'reverse', 'keys', 'upper', 'strip', 'split',
         'subclass-copy', 'clear')
if KIND not in KINDS:
    raise ValueError('unknown WEAVEPY_SAVED_NATIVE_KIND: ' + KIND)


class Child(list):
    pass


def bench(n):
    if n < 0:
        raise ValueError('work must be nonnegative')
    receiver = [2, 3, 5]
    if KIND == 'len':
        callback, expected = receiver.__len__, 3
    elif KIND == 'copy':
        callback, expected = receiver.copy, [2, 3, 5]
    elif KIND == 'reverse':
        callback, expected = receiver.reverse, None
    elif KIND == 'keys':
        receiver = {'first': 2, 'second': 3}
        callback, expected = receiver.keys, ['first', 'second']
    elif KIND == 'upper':
        callback, expected = 'aB\u03b3'.upper, 'AB\u0393'
    elif KIND == 'strip':
        callback, expected = ' Ab '.strip, 'Ab'
    elif KIND == 'split':
        callback, expected = 'a b c'.split, ['a', 'b', 'c']
    elif KIND == 'subclass-copy':
        receiver = Child(receiver)
        callback, expected = receiver.copy, [2, 3, 5]
    else:
        callback, expected = receiver.clear, None
    result = None
    for unused in range(n):
        result = callback()
    if n:
        assert (list(result) if KIND == 'keys' else result) == expected
    if KIND == 'reverse':
        assert receiver == ([5, 3, 2] if n % 2 else [2, 3, 5])
    elif KIND == 'clear':
        assert receiver == ([] if n else [2, 3, 5])
    return result


if __name__ == '__main__':
    start = time.perf_counter_ns()
    bench(int(os.environ.get('WEAVEPY_BENCH_WORK', '100000')))
    print('WEAVEPY_BENCH_NS=%d' % (time.perf_counter_ns() - start))
