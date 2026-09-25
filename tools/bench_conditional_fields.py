#!/usr/bin/env python3
"""Measure selected-field getters with setup and result checks timed."""
import os
import time

KIND = os.environ.get('WEAVEPY_CONDITIONAL_FIELD_KIND', 'object-left')
if KIND not in ('object-left', 'object-right', 'scalar-left', 'scalar-right',
                'slot-left', 'slot-right', 'float-left', 'float-right',
                'string-left', 'string-right'):
    raise ValueError('unknown WEAVEPY_CONDITIONAL_FIELD_KIND: ' + KIND)


class Config:
    FORWARD = 1


class Item:
    def __init__(self, direction, left, right):
        self.direction, self.left, self.right = direction, left, right


class SlotItem:
    __slots__ = ('direction', 'left', 'right')
    def __init__(self, direction, left, right):
        self.direction, self.left, self.right = direction, left, right


class Value:
    def __init__(self, value):
        self.value = value


def selected(item):
    if item.direction == Config.FORWARD:
        return item.left
    return item.right


def count_objects(item, n):
    total = 0
    for _ in range(n):
        total += selected(item).value
    return total


def count_scalars(item, n):
    total = 0
    for _ in range(n):
        total += selected(item)
    return total


def bench(n):
    left_branch = KIND.endswith('-left')
    if KIND.startswith('float-'):
        Config.FORWARD = 1.5
        direction = 1.5 if left_branch else 2.5
    elif KIND.startswith('string-'):
        Config.FORWARD = 'left'
        direction = 'left' if left_branch else 'right'
    else:
        direction = 1 if left_branch else 2
    if KIND.startswith('scalar-'):
        item = Item(direction, 3, 5)
        result = count_scalars(item, n)
    else:
        cls = SlotItem if KIND.startswith('slot-') else Item
        item = cls(direction, Value(3), Value(5))
        result = count_objects(item, n)
    assert result == n * (3 if left_branch else 5), result
    return result


if __name__ == '__main__':
    work = int(os.environ.get('WEAVEPY_BENCH_WORK', '200000'))
    start = time.perf_counter_ns()
    bench(work)
    print('WEAVEPY_BENCH_NS=%d' % (time.perf_counter_ns() - start))
