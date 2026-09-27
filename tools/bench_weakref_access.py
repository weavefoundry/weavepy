"""Measure weakref access with observable referent and container lifetimes."""
import gc
import os
import time
import weakref

KIND = os.environ.get('WEAVEPY_WEAKREF_KIND', 'ref')
if KIND not in ('ref', 'subclass', 'proxy', 'callable-proxy', 'weak-key', 'weak-value'):
    raise ValueError('unknown WEAVEPY_WEAKREF_KIND: ' + KIND)


class Node:
    def __init__(self, value):
        self.value = value


class CallableNode(Node):
    def __call__(self, extra):
        return self.value + extra


class Ref(weakref.ref):
    __slots__ = ('tag',)


CLEARED = 0


def witness(reference):
    global CLEARED
    assert reference() is None
    CLEARED += 1


def check_count(n):
    if n < 0:
        raise ValueError('work must be nonnegative')


def refs(n):
    check_count(n)
    owner = Node(7)
    reference = weakref.ref(owner) if KIND == 'ref' else Ref(owner)
    if KIND == 'subclass':
        reference.tag = 19
    total = 0
    for _ in range(n):
        total += reference().value
    assert total == 7 * n
    if KIND == 'subclass':
        assert reference.tag == 19
    del owner
    gc.collect()
    assert reference() is None
    return total


def proxies(n):
    check_count(n)
    owner = Node(7)
    reference = weakref.ref(owner)
    proxy = weakref.proxy(owner)
    total = 0
    for _ in range(n):
        total += proxy.value
    assert total == 7 * n
    del owner
    gc.collect()
    assert reference() is None
    try:
        proxy.value
    except ReferenceError:
        pass
    else:
        raise AssertionError('proxy retained its referent')
    return total


def callable_proxies(n):
    check_count(n)
    owner = CallableNode(7)
    reference = weakref.ref(owner)
    proxy = weakref.proxy(owner)
    total = 0
    for index in range(n):
        total += proxy(index & 7)
    rest = n % 8
    assert total == 7 * n + (n // 8) * 28 + rest * (rest - 1) // 2
    del owner
    gc.collect()
    assert reference() is None
    try:
        proxy(0)
    except ReferenceError:
        pass
    else:
        raise AssertionError('callable proxy retained its referent')
    return total


def weak_keys(n):
    global CLEARED
    check_count(n)
    CLEARED = 0
    owners = [Node(index) for index in range(32)]
    # Callback-bearing witnesses do not populate the basic-ref lookup cache.
    references = [weakref.ref(owner, witness) for owner in owners]
    mapping = weakref.WeakKeyDictionary((owner, index) for index, owner in enumerate(owners))
    total = 0
    for index in range(n):
        total += mapping[owners[index & 31]]
    rest = n % 32
    assert total == (n // 32) * 496 + rest * (rest - 1) // 2
    owners.clear()
    gc.collect()
    assert all(reference() is None for reference in references)
    assert len(mapping) == 0
    assert CLEARED == 32
    return total


def weak_values(n):
    global CLEARED
    check_count(n)
    CLEARED = 0
    owners = [Node(index) for index in range(32)]
    # Callback-bearing witnesses do not populate the basic-ref lookup cache.
    references = [weakref.ref(owner, witness) for owner in owners]
    mapping = weakref.WeakValueDictionary(enumerate(owners))
    total = 0
    for index in range(n):
        total += mapping[index & 31].value
    rest = n % 32
    assert total == (n // 32) * 496 + rest * (rest - 1) // 2
    owners.clear()
    gc.collect()
    assert all(reference() is None for reference in references)
    assert len(mapping) == 0
    assert CLEARED == 32
    return total


bench = {
    'ref': refs,
    'subclass': refs,
    'proxy': proxies,
    'callable-proxy': callable_proxies,
    'weak-key': weak_keys,
    'weak-value': weak_values,
}[KIND]

if __name__ == '__main__':
    start = time.perf_counter_ns()
    bench(int(os.environ.get('WEAVEPY_BENCH_WORK', '100000')))
    print('WEAVEPY_BENCH_NS=%d' % (time.perf_counter_ns() - start))
