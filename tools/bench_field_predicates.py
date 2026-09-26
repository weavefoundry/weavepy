#!/usr/bin/env python3
"""Measure repeated field predicates with setup and result checks timed."""
import os
import time

KIND = os.environ.get('WEAVEPY_FIELD_PREDICATE_KIND', 'integer')
if KIND not in ('integer', 'slots', 'float', 'class', 'string', 'mixed'):
    raise ValueError('unknown WEAVEPY_FIELD_PREDICATE_KIND: ' + KIND)

class Box:
    def __init__(self, value):
        self.value = value

class Slots:
    __slots__ = ('value',)
    def __init__(self, value):
        self.value = value

class ClassLeft:
    value = 1

class ClassRight:
    value = 2


def stronger(left, right):
    return left.value < right.value


def count(left, right, n):
    total = 0
    for _ in range(n):
        if stronger(left, right):
            total += 1
    return total


def bench(n):
    if KIND == 'slots':
        left, right = Slots(1), Slots(2)
    elif KIND == 'float':
        left, right = Box(1.5), Box(2.5)
    elif KIND == 'string':
        left, right = Box('alpha'), Box('beta')
    elif KIND == 'mixed':
        left, right = Box(1), Box(2.5)
    elif KIND == 'class':
        left, right = ClassLeft(), ClassRight()
    else:
        left, right = Box(1), Box(2)
    result = count(left, right, n)
    assert result == n, result
    return result

if __name__ == '__main__':
    work = int(os.environ.get('WEAVEPY_BENCH_WORK', '200000'))
    start = time.perf_counter_ns()
    bench(work)
    print('WEAVEPY_BENCH_NS=%d' % (time.perf_counter_ns() - start))
