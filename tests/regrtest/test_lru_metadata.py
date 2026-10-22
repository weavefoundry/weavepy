"""A wrapper's state stays out of its `__dict__`, which may change freely.

CPython keeps an `lru_cache` wrapper's cache, counters, and function in
its C struct: the instance dictionary holds only what `update_wrapper`
copies (and `cache_parameters`), and the wrapper never reads it.
"""

import functools
import gc

for limit in (0, 3, None):
    calls = []

    @functools.lru_cache(limit)
    def cached(key):
        calls.append(key)
        return key + 1

    assert cached(1) == 2
    assert cached(1) == 2
    assert len(calls) == (2 if limit == 0 else 1)
    values = vars(cached)
    assert not [name for name in values if name.startswith('_lru')], sorted(values)
    events = []

    class Name:
        def __init__(self, name):
            self.name = name

        def __hash__(self):
            return hash(self.name)

        def __eq__(self, other):
            events.append((self.name, other))
            return self.name == other

    # Rekey every entry by an owner whose `__eq__` records its calls: the
    # wrapper consults none of them.
    owners = []
    for name in list(values):
        value = values.pop(name)
        owner = Name(name)
        owners.append(owner)
        values[owner] = value
    before = cached.cache_info()
    assert cached(1) == 2
    after = cached.cache_info()
    if limit == 0:
        assert after.misses == before.misses + 1
    else:
        assert after.hits == before.hits + 1
    assert not events, events
    assert all(any(key is owner for key in values) for owner in owners)
    cached.cache_clear()
    assert cached.cache_info() == (0, 0, limit, 0)
    assert all(any(key is owner for key in values) for owner in owners)
    assert cached(2) == 3
    # An emptied dictionary leaves the cache and its counters working.
    cached.cache_clear()
    values.clear()
    assert cached(4) == 5
    assert cached(4) == 5
    assert cached.cache_info() == ((0, 2, 0, 0) if limit == 0 else (1, 1, limit, 1))
    gc.collect()


# The wrapper calls the function it was built with, whatever `__wrapped__`
# says, and a cached function wrapped again keeps a cache of its own.
def double(x):
    return 2 * x


inner = functools.lru_cache(maxsize=8)(double)
outer = functools.lru_cache(maxsize=8)(inner)
assert outer.__wrapped__ is inner
assert inner(2) == 4
assert outer(2) == 4
assert inner.cache_info() == (1, 1, 8, 1), inner.cache_info()
assert outer.cache_info() == (0, 1, 8, 1), outer.cache_info()
outer.__wrapped__ = lambda x: -x
assert outer(3) == 6
assert inner.cache_info() == (1, 2, 8, 2), inner.cache_info()


@functools.wraps(inner)
def passthrough(x):
    return inner(x)


assert not [name for name in vars(passthrough) if name.startswith('_lru')]
assert passthrough(2) == 4
assert inner.cache_info() == (2, 2, 8, 2), inner.cache_info()
print('LRU state outside the instance dictionary: ok')
