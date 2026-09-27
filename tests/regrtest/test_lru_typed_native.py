"""Typed native cache keys preserve recency, types, callbacks, and owners."""
import functools
import sys
import weakref

is_weavepy = sys.implementation.name == 'weavepy'


def check_order(capacity, kind):
    calls = 0

    @functools.lru_cache(capacity, typed=True)
    def cached(*args, **kwargs):
        nonlocal calls
        calls += 1
        return calls

    order = []
    results = {}
    hits = misses = 0
    for index in range(160):
        number = (index * 17 + index // 3) % 37
        kwargs = {}
        if kind == 'int':
            args = (number,)
        elif kind == 'str':
            args = (str(number),)
        elif kind == 'long':
            args = (10**30 + number,)
        elif kind == 'tuple':
            args = ((number, (str(number), 10**30 + number)),)
        elif kind == 'multi':
            args = (number, str(number))
        elif kind == 'keywords':
            args = ()
            kwargs = {'number': number, 'label': str(number)}
        else:
            args = ((number,),)
            kwargs = {'extra': str(number)}
        key = (args, tuple(kwargs.items()), tuple(map(type, args)),
               tuple(map(type, kwargs.values())))
        if key in results:
            expected = results[key]
            order.remove(key)
            hits += 1
        else:
            misses += 1
            expected = misses
            if capacity and len(order) == capacity:
                del results[order.pop(0)]
            if capacity != 0:
                results[key] = expected
        if capacity != 0:
            order.append(key)
        assert cached(*args, **kwargs) == expected, (capacity, kind, index)
        assert calls == misses
        assert cached.cache_info() == (hits, misses, capacity, len(order))
    if is_weavepy and capacity:
        assert len(cached._lru_state) == 1
        assert isinstance(cached._lru_state[0], bytearray)
    assert cached.cache_parameters() == {'maxsize': capacity, 'typed': True}
    cached.cache_clear()
    assert cached.cache_info() == (0, 0, capacity, 0)
    before = calls
    assert cached() == before + 1
    assert cached() == before + (2 if capacity == 0 else 1)


for capacity in (0, 1, 2, 4, 31, 128, None):
    for kind in ('int', 'str', 'long', 'tuple', 'multi', 'keywords', 'mixed'):
        check_order(capacity, kind)


# Appended argument types count toward the same bounded admission budget.
for count, admitted in ((31, True), (32, False)):
    @functools.lru_cache(2, typed=True)
    def many(*args):
        return len(args)

    args = tuple(range(count))
    assert many(*args) == many(*args) == count
    assert many.cache_info() == (1, 1, 2, 1)
    if is_weavepy:
        assert isinstance(many._lru_state, tuple) == admitted


# The typed flag survives unsupported-key demotion and subsequent clearing.
@functools.lru_cache(8, typed=True)
def kinds(key):
    return type(key)


for _ in range(3):
    for key in (1, 1.0, True, '1'):
        assert kinds(key) is type(key)
    for key in (1, 1.0, True, '1'):
        assert kinds(key) is type(key)
    assert kinds.cache_info() == (4, 4, 8, 4)
    if is_weavepy:
        assert kinds._lru_state is True
    kinds.cache_clear()


# typed=True distinguishes immediate argument types, not nested tuple types.
@functools.lru_cache(2, typed=True)
def nested(key):
    return key


first = tuple([1])
assert nested(first) is first
assert nested((1.0,)) is first
assert nested((True,)) is first
assert nested.cache_info() == (2, 1, 2, 1)


# Preserve stored owners through equal-but-distinct native tuple hits.
@functools.lru_cache(2, typed=True)
def owners(key):
    return 19


first = tuple([17, tuple(['owner', 10**30])])
second = tuple([17, tuple(['owner', 10**30])])
assert first is not second and first == second
assert owners(first) == owners(second) == 19
assert owners.cache_info() == (1, 1, 2, 1)
if is_weavepy:
    assert next(iter(owners._lru_cache))[0] is first


# Recursive miss insertion wins, and clearing doesn't discard typedness.
once = True


@functools.lru_cache(3, typed=True)
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


# Demotion during a native-key miss retains both logical recency and types.
for unsupported in (4.0, False, b'four'):
    calls = []

    @functools.lru_cache(3, typed=True)
    def transition(key):
        calls.append(key)
        if key == 99:
            transition(unsupported)
        return type(key), key

    for key in (0, 1, 2, 0, 3, 2):
        transition(key)
    assert len(calls) == 4
    transition(unsupported)
    transition(3)
    transition(2)
    assert len(calls) == 5
    transition(0)
    assert len(calls) == 6
    nested_cache = functools.lru_cache(3, typed=True)(transition.__wrapped__)
    transition = nested_cache
    for key in (0, 1, 2):
        transition(key)
    assert transition(99) == (int, 99)
    assert transition.cache_info() == (0, 5, 3, 3)
    assert transition(2) == (int, 2) and transition(99) == (int, 99)
    assert transition.cache_info() == (2, 5, 3, 3)


# Hashing argument classes can execute Python before cache bookkeeping.
callbacks = []


class Meta(type):
    def __hash__(cls):
        callbacks.append('class hash')
        callback_cache(0)
        return 73


class Key(metaclass=Meta):
    def __hash__(self):
        callbacks.append('hash')
        return 11

    def __eq__(self, other):
        callbacks.append('eq')
        callback_cache(0)
        return isinstance(other, Key)


@functools.lru_cache(8, typed=True)
def callback_cache(key):
    return 19


for number in range(8):
    callback_cache(number)
assert callback_cache(Key()) == 19
callbacks.clear()
assert callback_cache(Key()) == 19
assert callbacks == ['hash', 'class hash', 'eq'], callbacks
if is_weavepy:
    assert callback_cache._lru_state is True


class BrokenMeta(type):
    def __hash__(cls):
        raise ValueError('class hash failed')


class Broken(metaclass=BrokenMeta):
    pass


before = callback_cache.cache_info()
try:
    callback_cache(Broken())
except ValueError as error:
    assert str(error) == 'class hash failed'
else:
    raise AssertionError('class hash exception lost')
assert callback_cache.cache_info() == before


# Private stored-owner tampering must fail before a paired borrow can release it.
if is_weavepy:
    class IntOwner(int):
        pass

    @functools.lru_cache(2, typed=True)
    def tampered(key):
        return 19

    assert tampered(7) == 19
    cache = tampered._lru_cache
    cached_key = next(iter(cache))
    value = cache.pop(cached_key)
    owner = IntOwner(7)
    cache[(owner, int)] = value
    before = tampered.cache_info()
    try:
        tampered(7)
    except TypeError as error:
        assert 'inconsistent private recency data' in str(error)
    else:
        raise AssertionError('foreign stored owner bypassed guard')
    assert tampered.cache_info() == before
    assert next(iter(cache))[0] is owner

    @functools.lru_cache(2, typed=True)
    def pinned(key):
        return type(key)

    fields = set(pinned.__dict__)
    assert pinned(0) is int
    assert set(pinned.__dict__) == fields
    storage = pinned._lru_state[0]
    view = memoryview(storage)
    before = bytes(storage)
    assert pinned(0) is int
    for operation in (lambda: pinned(1), lambda: pinned(1.0), pinned.cache_clear):
        try:
            operation()
        except BufferError:
            pass
        else:
            raise AssertionError('resized exported typed recency storage')
        assert bytes(storage) == before
        assert pinned.cache_info().currsize == 1
    view.release()
    pinned.cache_clear()
    assert pinned(1) is int
    assert pinned(1.0) is float
    assert pinned._lru_state is True and len(storage) == 0
    pinned.cache_clear()
    assert pinned(1) is int and pinned(1.0) is float


# Eviction and clear publish state before finalizers run in the real caller.
seen = []


class Value:
    def __init__(self, number):
        self.number = number

    def __del__(self):
        seen.append((self.number, sys._getframe(1).f_code.co_name,
                     finalizing.cache_info().currsize))


@functools.lru_cache(1, typed=True)
def finalizing(key):
    return Value(key)


witness = weakref.ref(finalizing(1))


def typed_evict():
    finalizing(2)
    assert witness() is None
    assert seen == [(1, 'typed_evict', 1)], seen


def typed_clear():
    finalizing.cache_clear()
    assert seen == [(1, 'typed_evict', 1), (2, 'typed_clear', 0)], seen


typed_evict()
typed_clear()
print('Typed native LRU recency, mode, callbacks, and owners: ok')
