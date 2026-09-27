"""Native tuple keys preserve bounded LRU order, owners, and fallback."""

import functools
import sys
import weakref

is_weavepy = sys.implementation.name == 'weavepy'


def check_order(capacity, kind):
    calls = 0

    @functools.lru_cache(capacity)
    def cached(*args, **kwargs):
        nonlocal calls
        calls += 1
        return calls

    order = []
    results = {}
    hits = misses = 0
    for index in range(400):
        number = (index * 17 + index // 3) % (capacity * 2 + 1)
        kwargs = {}
        if kind == 'tuple':
            args = (tuple([number, str(number)]),)
        elif kind == 'nested':
            args = ((number, (str(number), 10**30 + number)),)
        elif kind == 'multi':
            args = (number, str(number), 10**30 + number)
        elif kind == 'keywords':
            args = ()
            kwargs = {'number': number, 'label': str(number)}
        elif kind == 'mixed':
            args = ((number, str(number)),)
            kwargs = {'extra': (number,)}
        else:
            # Distinct native integers with equal Python hashes.
            args = (number * sys.hash_info.modulus, -1 if index % 2 else -2)
        key = (args, tuple(kwargs.items()))
        if key in results:
            expected = results[key]
            order.remove(key)
            hits += 1
        else:
            misses += 1
            expected = misses
            if len(order) == capacity:
                del results[order.pop(0)]
            results[key] = expected
        order.append(key)
        assert cached(*args, **kwargs) == expected, (capacity, kind, index)
        assert cached.cache_info() == (hits, misses, capacity, len(order))
    if is_weavepy:
        assert isinstance(cached._lru_state, bytearray), kind
    cached.cache_clear()
    assert cached.cache_info() == (0, 0, capacity, 0)
    assert cached() == calls
    assert cached() == calls
    assert cached.cache_info() == (1, 1, capacity, 1)


for capacity in (1, 2, 3, 7, 31):
    for kind in ('tuple', 'nested', 'multi', 'keywords', 'mixed', 'collision'):
        check_order(capacity, kind)


@functools.lru_cache(4)
def keywords(**kwargs):
    return tuple(kwargs.items())


assert keywords(a=1, b=2) == (('a', 1), ('b', 2))
assert keywords(b=2, a=1) == (('b', 2), ('a', 1))
assert keywords(a=1, b=2) == (('a', 1), ('b', 2))
assert keywords.cache_info() == (1, 2, 4, 2)


# Equal new tuples hit without replacing the cached tuple owner.
@functools.lru_cache(2)
def owners(key):
    return 19


first = tuple([17, tuple(['owner', 10**30])])
second = tuple([17, tuple(['owner', 10**30])])
assert first is not second and first == second
assert owners(first) == owners(second) == 19
assert owners.cache_info() == (1, 1, 2, 1)
if is_weavepy:
    assert next(iter(owners._lru_cache))[0] is first


# Clear and recursive insertion can occur during a tuple-key miss.
once = True


@functools.lru_cache(3)
def recursive(a, b):
    global once
    if a == 20 and once:
        once = False
        return recursive(a, b)
    if a == 99:
        recursive.cache_clear()
    return a + b


for number in range(15):
    assert recursive(number, 1) == number + 1
assert recursive(20, 1) == 21
assert recursive(20, 1) == 21
assert recursive.cache_info() == (1, 17, 3, 3)
assert recursive(99, 1) == 100
assert recursive.cache_info() == (0, 0, 3, 1)
assert recursive(99, 1) == 100
assert recursive.cache_info() == (1, 0, 3, 1)


# Demote in logical order, including during an outer native-key miss.
for unsupported in ((4.0,), (False, 'four'), (b'four',)):
    calls = []

    @functools.lru_cache(3)
    def transition(key):
        calls.append(key)
        if key == (99,):
            transition(unsupported)
        return key

    for key in ((0,), (1,), (2,), (0,), (3,), (2,)):
        transition(key)
    assert len(calls) == 4
    transition(unsupported)
    transition((3,))
    transition((2,))
    assert len(calls) == 5
    transition((0,))
    assert len(calls) == 6
    if is_weavepy:
        assert transition._lru_state is None

    # A fresh wrapper starts native and demotes during its wrapped call.
    nested = functools.lru_cache(3)(transition.__wrapped__)
    transition = nested
    for key in ((0,), (1,), (2,)):
        nested(key)
    assert nested((99,)) == (99,)
    assert nested.cache_info() == (0, 5, 3, 3)
    assert nested((2,)) == (2,) and nested((99,)) == (99,)
    assert nested.cache_info() == (2, 5, 3, 3)


# Admission is bounded; larger native keys remain valid through fallback.
def depth_key(depth):
    key = 7
    for _ in range(depth):
        key = (key,)
    return key


for args, admitted in (
    (tuple(range(63)), True),
    (tuple(range(64)), False),
    ((depth_key(7),), True),
    ((depth_key(8),), False),
):
    @functools.lru_cache(3)
    def bounded(*values):
        return len(values)

    assert bounded(*args) == len(args)
    assert bounded(*args) == len(args)
    assert bounded.cache_info() == (1, 1, 3, 1)
    if is_weavepy:
        assert isinstance(bounded._lru_state, bytearray) == admitted


callbacks = []


class Key:
    def __hash__(self):
        callbacks.append('hash')
        return 11

    def __eq__(self, other):
        callbacks.append('eq')
        callback_cache((0,))
        return isinstance(other, Key)


@functools.lru_cache(8)
def callback_cache(key):
    return 19


for number in range(8):
    callback_cache((number,))
assert callback_cache((Key(),)) == 19
callbacks.clear()
assert callback_cache((Key(),)) == 19
assert callbacks == ['hash', 'eq'], callbacks
if is_weavepy:
    assert callback_cache._lru_state is None


class BrokenHash:
    def __hash__(self):
        raise ValueError('hash failed')


before = callback_cache.cache_info()
try:
    callback_cache((BrokenHash(),))
except ValueError as error:
    assert str(error) == 'hash failed'
else:
    raise AssertionError('hash exception lost')
assert callback_cache.cache_info() == before


# Native-payload subclasses still require the owner-safe fallback.
class IntOwner(int):
    pass


class TupleOwner(tuple):
    pass


for key, equal_key in (((IntOwner(7),), (7,)), (TupleOwner((7,)), (7,))):
    @functools.lru_cache(3)
    def subclass(key):
        return 19

    assert subclass(key) == subclass(equal_key) == 19
    assert subclass.cache_info() == (1, 1, 3, 1)
    if is_weavepy:
        assert subclass._lru_state is None


# A private-cache replacement can hide a subclass under an exact tuple.
# Even a native equality result must not bypass the stored-owner guard.
if is_weavepy:
    @functools.lru_cache(2)
    def tampered(key):
        return 19

    assert tampered((7,)) == 19
    cache = tampered._lru_cache
    cached_key = next(iter(cache))
    value = cache.pop(cached_key)
    owner = IntOwner(7)
    replacement = ((owner,),)
    cache[replacement] = value
    before = tampered.cache_info()
    try:
        tampered((7,))
    except TypeError as error:
        assert 'inconsistent private recency data' in str(error)
    else:
        raise AssertionError('foreign stored tuple owner bypassed guard')
    assert tampered.cache_info() == before
    assert next(iter(cache))[0][0] is owner


# Tuple recency must publish consistent state before value finalizers run.
seen = []


class Value:
    def __init__(self, number):
        self.number = number

    def __del__(self):
        seen.append((self.number, sys._getframe(1).f_code.co_name,
                     finalizing.cache_info().currsize))


@functools.lru_cache(1)
def finalizing(key):
    return Value(key[0])


witness = weakref.ref(finalizing((1,)))


def native_tuple_evict():
    finalizing((2,))
    assert witness() is None
    assert seen == [(1, 'native_tuple_evict', 1)], seen


def native_tuple_clear():
    finalizing.cache_clear()
    assert seen == [(1, 'native_tuple_evict', 1),
                    (2, 'native_tuple_clear', 0)], seen


native_tuple_evict()
native_tuple_clear()
print('Native tuple LRU order, bounded admission, callbacks, and owners: ok')
