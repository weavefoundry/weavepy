"""Metadata mutations must remain visible, with equal stored keys retained."""

import functools
import gc
import sys

for limit in (0, 3, None):
    calls = []

    @functools.lru_cache(limit)
    def cached(key):
        calls.append(key)
        return key + 1

    assert cached(1) == 2
    assert cached(1) == 2
    assert len(calls) == (2 if limit == 0 else 1)
    if sys.implementation.name != 'weavepy':
        continue
    values = vars(cached)
    events = []

    class Name:
        def __init__(self, name):
            self.name = name

        def __hash__(self):
            return hash(self.name)

        def __eq__(self, other):
            events.append((self.name, other))
            return self.name == other

    owners = []
    for name in (
        '__wrapped__', '_lru_maxsize', '_lru_cache', '_lru_state',
        '_lru_cache_info_cls', '_lru_hits', '_lru_misses',
    ):
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
    assert events
    assert all(any(key is owner for key in values) for owner in owners)
    cached.cache_clear()
    assert cached.cache_info() == (0, 0, limit, 0)
    assert all(any(key is owner for key in values) for owner in owners)
    assert cached(2) == 3
    # Missing counters and scalar state retain their initialization behavior.
    cached.cache_clear()
    del values['_lru_hits']
    del values['_lru_misses']
    if limit == 3:
        del values['_lru_state']
    assert cached(4) == 5
    assert cached(4) == 5
    assert cached.cache_info() == ((0, 2, 0, 0) if limit == 0 else (1, 1, limit, 1))
    gc.collect()

print('LRU metadata mutations and stored key owners: ok')
