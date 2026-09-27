#!/usr/bin/env python3
"""Measure field getter and arithmetic calls with setup and checks timed."""
import os
import time

KIND = os.environ.get('WEAVEPY_FIELD_READ_KIND', 'slot-getter')
if KIND not in ('slot-getter', 'dict-getter', 'slot-add', 'dict-add',
                'slot-chain', 'dict-chain'):
    raise ValueError('unknown WEAVEPY_FIELD_READ_KIND: ' + KIND)


class Box:
    def __init__(self, value, other, link=None):
        self.value, self.other, self.link = value, other, link


class Slots:
    __slots__ = ('value', 'other', 'link')
    def __init__(self, value, other, link=None):
        self.value, self.other, self.link = value, other, link


def getter(item):
    return item.value


def add(item):
    return item.value + item.other


def chain(item):
    return item.link.value


def count(operation, item, n):
    total = 0
    for _ in range(n):
        total += operation(item)
    return total


def bench(n):
    cls = Slots if KIND.startswith('slot-') else Box
    if KIND.endswith('-getter'):
        operation, expected = getter, 3
    elif KIND.endswith('-add'):
        operation, expected = add, 8
    else:
        operation, expected = chain, 7
    item = cls(3, 5, cls(7, 11))
    result = count(operation, item, n)
    assert result == n * expected, result
    return result


if __name__ == '__main__':
    work = int(os.environ.get('WEAVEPY_BENCH_WORK', '200000'))
    start = time.perf_counter_ns()
    bench(work)
    print('WEAVEPY_BENCH_NS=%d' % (time.perf_counter_ns() - start))
