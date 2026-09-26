"""Unordered removal preserves membership, errors, ownership, and callbacks."""
import gc
import weakref


class SetSubclass(set):
    pass


class Key:
    def __init__(self, value):
        self.value = value

    def __hash__(self):
        return 7

    def __eq__(self, other):
        return isinstance(other, Key) and self.value == other.value


for factory in (set, SetSubclass):
    values = factory(range(97))
    for i in range(0, 97, 2):
        assert values.discard(i) is None
    assert values == set(range(1, 97, 2))
    for i in range(1, 97, 4):
        assert values.remove(i) is None
        assert values.add(i + 200) is None
    expected = set(range(3, 97, 4)) | set(range(201, 297, 4))
    assert values == expected
    assert values.difference_update(tuple(range(3, 97, 4))) is None
    assert values == set(range(201, 297, 4))
    popped = set()
    saved_pop = values.pop
    while values:
        value = saved_pop()
        assert value not in values and value not in popped
        popped.add(value)
    assert popped == set(range(201, 297, 4))
    try:
        saved_pop()
    except KeyError:
        pass
    else:
        raise AssertionError('empty set.pop did not raise')

    owners = [Key(i) for i in range(24)]
    values = factory(owners)
    for i in range(0, 24, 2):
        assert values.remove(Key(i)) is None
    assert {value.value for value in values} == set(range(1, 24, 2))
    for i in range(1, 24, 4):
        assert values.discard(Key(i)) is None
    assert {value.value for value in values} == set(range(3, 24, 4))
    missing = Key(101)
    try:
        values.remove(missing)
    except KeyError as error:
        assert error.args[0] is missing
    else:
        raise AssertionError('missing set.remove did not raise')

    values = factory([frozenset([1, 2])])
    assert values.remove(set([1, 2])) is None
    assert not values
    values = factory(range(5))
    iterator = iter(values)
    next(iterator)
    values.remove(0)
    try:
        next(iterator)
    except RuntimeError:
        pass
    else:
        raise AssertionError('set iterator missed size change')


events = []


class Life:
    def __init__(self, value):
        self.value = value

    def __del__(self):
        events.append(self.value)


values = {Life(i) for i in range(5)}
references = [weakref.ref(value) for value in values]
popped = values.pop()
witness = weakref.ref(popped)
gc.collect()
assert witness() is popped and not events
released = popped.value
del popped
gc.collect()
assert witness() is None and events == [released]
values.clear()
gc.collect()
assert sorted(events) == list(range(5))
assert all(reference() is None for reference in references)

hashes = []
equalities = []


class ObservedKey(Key):
    def __hash__(self):
        hashes.append(self.value)
        return 7

    def __eq__(self, other):
        equalities.append((self.value, other.value))
        return super().__eq__(other)


for factory in (set, SetSubclass):
    owners = [ObservedKey(i) for i in range(12)]
    values = factory(owners)
    identities = {id(value) for value in owners}
    hashes.clear()
    equalities.clear()
    for _ in owners:
        # Explicit builtin dispatch also returns the original stored object.
        popped = set.pop(values)
        identities.remove(id(popped))
    assert not values and not identities
    assert not hashes, ('pop rehashed keys', hashes)
    assert not equalities, ('pop compared keys', equalities)

print('set removals: membership, ownership, iteration, and pop callbacks ok')
