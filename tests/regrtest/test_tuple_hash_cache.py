"""Successful tuple hashes persist; failures and recycled storage don't."""

import gc
import functools
import threading
import weakref


def combine(hashes):
    mask = (1 << 64) - 1
    acc = 2870177450012600261
    for value in hashes:
        acc = (acc + (value & mask) * 14029467366897019727) & mask
        acc = ((acc << 31) | (acc >> 33)) & mask
        acc = (acc * 11400714785074694791) & mask
    acc = (acc + (len(hashes) ^ (2870177450012600261 ^ 3527539))) & mask
    if acc >= 1 << 63:
        acc -= 1 << 64
    return 1546275796 if acc == -1 else acc


for size in (0, 1, 2, 3, 4, 8, 16, 17, 65, 129):
    values = tuple(range(-size, 0))
    expected = combine([hash(value) for value in values])
    assert hash(values) == expected
    assert values.__hash__() == expected
    assert tuple.__hash__(values) == expected


class Counter:
    def __init__(self, value):
        self.value = value
        self.calls = 0
        self.fail = False

    def __hash__(self):
        self.calls += 1
        if self.fail:
            raise ValueError("tuple hash failed")
        return self.value


owner = Counter(-1)
key = (owner, 7)
expected = combine([-2, 7])
assert hash(key) == expected
owner.value = 99
owner.fail = True
assert key.__hash__() == expected
assert tuple.__hash__(key) == expected
assert hash(key) == expected
assert {key: "value"}[key] == "value"
assert key in {key}
assert key in frozenset((key,))
assert owner.calls == 1

# Keyed containers must populate the same cache as the hash builtin.
for make in (lambda key: {key: 1}, lambda key: {key}, lambda key: frozenset((key,))):
    owner = Counter(123)
    key = (owner,)
    container = make(key)
    assert owner.calls == 1
    owner.fail = True
    assert key in container
    assert hash(key) == combine([123])
    assert owner.calls == 1

# A successful cache remains authoritative after the element's class
# becomes unhashable, including nested tuples and table preconditions.
class BecomesUnhashable:
    def __hash__(self):
        return 456


owner = BecomesUnhashable()
key = (owner,)
expected = hash(key)
BecomesUnhashable.__hash__ = None
assert hash(key) == expected
assert key.__hash__() == expected
assert {key: 1}[key] == 1
assert key in {key}
assert hash((key, key)) == combine([expected, expected])

# Nested shared tuples retain their own hashes. Hashing the outer tuple
# must not recompute a shared child's elements on each occurrence.
owner = Counter(789)
inner = (owner,)
outer = (inner, inner, inner)
inner_hash = combine([789])
assert hash(outer) == combine([inner_hash, inner_hash, inner_hash])
assert owner.calls == 1
assert hash(inner) == inner_hash
assert owner.calls == 1


def expect_failure(operation):
    try:
        operation()
    except ValueError as error:
        assert str(error) == "tuple hash failed"
    else:
        raise AssertionError("failed tuple hash was accepted")


for operation in (
    hash,
    tuple.__hash__,
    lambda key: key.__hash__(),
    lambda key: {key: 1},
    lambda key: {key},
    lambda key: frozenset((key,)),
):
    owner = Counter(23)
    owner.fail = True
    later = Counter(24)
    key = (owner, later)
    for count in range(1, 4):
        expect_failure(lambda: operation(key))
        assert owner.calls == count
        assert later.calls == 0
    owner.fail = False
    assert hash(key) == combine([23, 24])
    assert owner.calls == 4 and later.calls == 1
    owner.fail = True
    assert hash(key) == combine([23, 24])
    assert owner.calls == 4 and later.calls == 1

# The tuple free list must discard the previous allocation's cache.
for value in range(300):
    pair = (value, -value)
    assert hash(pair) == combine([hash(value), hash(-value)])
    del pair


class TupleOverride(tuple):
    calls = 0

    def __hash__(self):
        TupleOverride.calls += 1
        return 1000 + TupleOverride.calls


subclass = TupleOverride((1, 2))
assert tuple.__hash__(subclass) == combine([1, 2])
assert hash(subclass) == 1001
assert hash(subclass) == 1002

# Cached hashes don't add ownership of the tuple or its elements.
finalized = []


class Leaf:
    def __hash__(self):
        return 12

    def __del__(self):
        finalized.append("leaf")


gc.collect()
gc.disable()
leaf = Leaf()
reference = weakref.ref(leaf)
key = (leaf,)
assert hash(key) == combine([12])
del leaf, key
assert reference() is None
assert finalized == ["leaf"]
gc.enable()

# Successful publication is safe when a tuple is shared by native threads.
shared = tuple(range(100))
expected = combine(list(range(100)))
errors = []


def worker():
    for _ in range(100):
        if hash(shared) != expected:
            errors.append("hash changed")


threads = [threading.Thread(target=worker) for _ in range(4)]
for thread in threads:
    thread.start()
for thread in threads:
    thread.join()
assert errors == []

# Native and Python cache wrappers must use the same successful tuple hash
# for lookup and insertion. Hash errors precede the function and counters.
for maxsize in (1, None):
    calls = []

    @functools.lru_cache(maxsize=maxsize)
    def cached(owner, value):
        calls.append(value)
        return value + 1

    owner = Counter(67)
    owner.fail = True
    expect_failure(lambda: cached(owner, 3))
    assert owner.calls == 1
    assert calls == []
    assert cached.cache_info().hits == 0
    assert cached.cache_info().misses == 0
    owner.fail = False
    assert cached(owner, 3) == 4
    assert cached(owner, 3) == 4
    assert owner.calls == 3
    assert calls == [3]
    assert cached.cache_info().hits == 1
    assert cached.cache_info().misses == 1


class Collision:
    def __hash__(self):
        return 7

    def __eq__(self, other):
        raise ValueError("tuple hash failed")


calls = []


@functools.lru_cache(maxsize=2)
def collision_cache(owner, value):
    calls.append(value)
    return value


assert collision_cache(Collision(), 1) == 1
expect_failure(lambda: collision_cache(Collision(), 1))
assert calls == [1]
print("ok")
