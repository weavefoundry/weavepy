"""Time descriptor and lookup-hook argument fallbacks, including transitions."""
import os
import time

KIND = os.environ.get('WEAVEPY_FIELD_ARGUMENT_FALLBACK', 'descriptor')
if KIND not in ('descriptor', 'hook', 'descriptor-transition', 'hook-transition'):
    raise ValueError('unknown field argument fallback: ' + KIND)


class Value:
    def __init__(self, rank):
        self.rank = rank


class Node:
    def __init__(self, left, right):
        self.left, self.right = left, right


def stronger(left, right):
    return left.rank < right.rank


def count(root, n):
    total = 0
    for _ in range(n):
        if stronger(root.left, root.right.left):
            total += 1
    return total


def bench(n):
    class Root(Node):
        pass
    root = Root(Value(1), Node(Value(2), None))
    root.saved_left = root.left
    if KIND.endswith('-transition'):
        assert count(root, 100) == 100
    if KIND.startswith('descriptor'):
        Root.left = property(lambda self: self.saved_left)
    else:
        def getattribute(self, name):
            if name == 'left':
                return object.__getattribute__(self, 'saved_left')
            return object.__getattribute__(self, name)
        Root.__getattribute__ = getattribute
    result = count(root, n)
    assert result == n, result
    return result


if __name__ == '__main__':
    work = int(os.environ.get('WEAVEPY_BENCH_WORK', '200000'))
    start = time.perf_counter_ns()
    bench(work)
    print('WEAVEPY_BENCH_NS=%d' % (time.perf_counter_ns() - start))
