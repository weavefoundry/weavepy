"""Measure complete callers passing cached fields to small functions."""
import os
import time

KIND = os.environ.get('WEAVEPY_FIELD_ARGUMENT_KIND', 'global-dict')
if KIND not in ('global-dict', 'static-dict', 'method-dict',
                'global-slot', 'static-slot', 'method-slot'):
    raise ValueError('unknown field argument kind: ' + KIND)


class Item:
    def __init__(self, rank):
        self.rank = rank


class SlotItem:
    __slots__ = ('rank',)
    def __init__(self, rank):
        self.rank = rank


class Root:
    def __init__(self, left, right, rules):
        self.left, self.right, self.rules = left, right, rules


class SlotRoot:
    __slots__ = ('left', 'right', 'rules')
    def __init__(self, left, right, rules):
        self.left, self.right, self.rules = left, right, rules


def stronger(left, right):
    return left.rank < right.rank


class Rules:
    @staticmethod
    def stronger(left, right):
        return left.rank < right.rank

    def compare(self, left, right):
        return left.rank < right.rank


def global_calls(root, n):
    total = 0
    for _ in range(n):
        if stronger(root.left, root.right.left):
            total += 1
    return total


def static_calls(root, n):
    total = 0
    for _ in range(n):
        if Rules.stronger(root.left, root.right.left):
            total += 1
    return total


def method_calls(root, rules, n):
    total = 0
    for _ in range(n):
        if rules.compare(root.left, root.right.left):
            total += 1
    return total


def bench(n):
    item = SlotItem if KIND.endswith('-slot') else Item
    node = SlotRoot if KIND.endswith('-slot') else Root
    rules = Rules()
    root = node(item(1), node(item(2), None, rules), rules)
    if KIND.startswith('global-'):
        result = global_calls(root, n)
    elif KIND.startswith('static-'):
        result = static_calls(root, n)
    else:
        result = method_calls(root, rules, n)
    assert result == n, result
    return result


if __name__ == '__main__':
    work = int(os.environ.get('WEAVEPY_BENCH_WORK', '200000'))
    start = time.perf_counter_ns()
    bench(work)
    print('WEAVEPY_BENCH_NS=%d' % (time.perf_counter_ns() - start))
