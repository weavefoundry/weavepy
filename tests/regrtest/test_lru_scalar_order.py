"""Bounded scalar caches preserve recency and callback-capable fallback."""
import functools
import sys


@functools.lru_cache(7)
def warm(key):
    return key


for _ in range(4000):
    assert warm(17) == 17
assert warm.cache_info() == (3999, 1, 7, 1)


def check_order(capacity, kind):
    calls = 0

    @functools.lru_cache(capacity)
    def cached(key):
        nonlocal calls
        calls += 1
        return key, calls

    order = []
    results = {}
    hits = 0
    misses = 0
    for i in range(500):
        number = (i * 17 + i // 3) % (capacity * 2 + 1)
        key = number if kind == 'int' else (str(number) if kind == 'str' else 10**30 + number)
        if key in results:
            expected = results[key]
            order.remove(key)
            hits += 1
        else:
            misses += 1
            expected = (key, misses)
            if len(order) == capacity:
                del results[order.pop(0)]
            results[key] = expected
        order.append(key)
        assert cached(key) == expected, (capacity, kind, i)
        assert cached.cache_info() == (hits, misses, capacity, len(order))
    cached.cache_clear()
    assert cached.cache_info() == (0, 0, capacity, 0)
    assert cached(1) == (1, calls)
    assert cached.cache_info() == (0, 1, capacity, 1)


for capacity in (1, 2, 3, 7, 31):
    for kind in ('int', 'str', 'long'):
        check_order(capacity, kind)


# Physical entry order differs from recency before changing the key shape.
# Exact native tuples stay in dense recency; floats and bools use fallback.
for next_key in ((4,), 4.0, False):
    calls = []

    @functools.lru_cache(3)
    def transition(key):
        calls.append(key)
        return key

    for key in (0, 1, 2, 0, 3, 2):
        transition(key)
    assert calls == [0, 1, 2, 3]
    transition(next_key)
    before = len(calls)
    transition(3)
    transition(2)
    assert len(calls) == before
    transition(0)
    assert calls[-1] == 0 and len(calls) == before + 1
    transition.cache_clear()
    assert transition(8) == 8


# A recursive miss may populate the same key or clear the scalar cache.
once = True


@functools.lru_cache(3)
def recursive(key):
    global once
    if key == 20 and once:
        once = False
        return recursive(key)
    return key + 1


for key in range(15):
    assert recursive(key) == key + 1
assert recursive(20) == 21 and recursive.cache_info().currsize == 3
assert recursive(20) == 21


@functools.lru_cache(3)
def clearing(key):
    if key == 9:
        clearing.cache_clear()
    return key


for key in (1, 2, 3, 9):
    assert clearing(key) == key
assert clearing.cache_info() == (0, 0, 3, 1)
assert clearing(9) == 9 and clearing.cache_info().hits == 1


# The wrapped function can change the key shape during an outer scalar miss.
@functools.lru_cache(3)
def changing(key):
    if key == 99:
        return changing((99,))
    return key


for key in (0, 1, 2):
    changing(key)
assert changing(99) == (99,)
assert changing.cache_info() == (0, 5, 3, 3)
assert changing(2) == 2 and changing(99) == (99,)
assert changing.cache_info() == (2, 5, 3, 3)


@functools.lru_cache(3)
def keyword(key):
    return key


for key in (0, 1, 2, 0):
    keyword(key)
assert keyword(key=7) == 7
assert keyword(key=7) == 7
assert keyword(7) == 7
assert keyword.cache_info() == (2, 5, 3, 3)


# Python hashing still happens once per call, outside the scalar borrow.
callbacks = []


class Key:
    def __init__(self, number):
        self.number = number

    def __hash__(self):
        callbacks.append('hash')
        return self.number

    def __eq__(self, other):
        caller = sys._getframe(1)
        callbacks.append((caller.f_code.co_name, caller.f_locals.get('marker')))
        callback_cache(0)
        return isinstance(other, Key) and self.number == other.number


@functools.lru_cache(8)
def callback_cache(key):
    return 19


def lookup(key):
    marker = 'cache caller'
    return callback_cache(key)


for key in range(8):
    callback_cache(key)
assert lookup(Key(11)) == 19
callbacks.clear()
assert lookup(Key(11)) == 19
assert callbacks == ['hash', ('lookup', 'cache caller')], callbacks

# Typed and keyword forms retain separate cache keys after scalar calls.
@functools.lru_cache(4, typed=True)
def typed(key):
    return type(key)


assert typed(1) is int and typed(1.0) is float
assert typed.cache_info() == (0, 2, 4, 2)
print('scalar LRU order, transitions, recursion, and callbacks: ok')

# Exporting private implementation storage must not permit an unsafe resize.
if sys.implementation.name == 'weavepy':
    @functools.lru_cache(2)
    def pinned(key):
        return key

    fields = set(pinned.__dict__)
    assert pinned(0) == 0
    assert set(pinned.__dict__) == fields
    storage = pinned._lru_state
    view = memoryview(storage)
    before = bytes(storage)
    assert pinned(0) == 0
    for operation in (lambda: pinned(1), lambda: pinned((1,)), pinned.cache_clear):
        try:
            operation()
        except BufferError:
            pass
        else:
            raise AssertionError('resized exported recency storage')
        assert bytes(storage) == before
        assert pinned.cache_info().currsize == 1
    view.release()
    assert pinned(1) == 1 and pinned.cache_info().currsize == 2
    pinned.cache_clear()
    assert pinned.cache_info() == (0, 0, 2, 0)
    assert pinned(0) == 0
    assert pinned(0.0) == 0.0
    assert pinned._lru_state is None and len(storage) == 0
    pinned.cache_clear()
    assert pinned(0) == 0 and pinned._lru_state is None
    print('scalar LRU exported storage: ok')
