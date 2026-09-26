"""Set call dispatch preserves callback fallback and returned owners."""
import gc
import sys
import weakref


def drain(values, count):
    total = 0
    for _ in range(count):
        total += values.pop()
    return total


def remove_all(values, count):
    for i in range(count):
        assert values.remove(i) is None
    return len(values)


assert drain(set(range(4000)), 4000) == 3999 * 4000 // 2
assert remove_all(set(range(4000)), 4000) == 0
for value in (None, True, 3001, 'key'):
    values = {value}
    assert values.remove(value) is None and not values
    try:
        values.remove(value)
    except KeyError as error:
        assert error.args[0] is value
    else:
        raise AssertionError('missing remove did not raise')
try:
    drain(set(), 1)
except KeyError:
    pass
else:
    raise AssertionError('empty pop did not raise')

callbacks = []


class Colliding:
    def __hash__(self):
        return 0

    def __eq__(self, other):
        frame = sys._getframe(1)
        callbacks.append((other, frame.f_code.co_name, frame.f_locals.get('marker')))
        if getattr(self, 'raises', False):
            raise ValueError('comparison failed')
        return other == 0


def remove_collision(values, key):
    marker = 'visible comparison caller'
    values.remove(key)
    return marker


owner = Colliding()
values = {owner}
assert remove_collision(values, 0) == 'visible comparison caller'
assert not values
assert callbacks == [(0, 'remove_collision', 'visible comparison caller')], callbacks
callbacks.clear()
owner.raises = True
values = {owner}
try:
    remove_collision(values, 0)
except ValueError as error:
    assert str(error) == 'comparison failed'
else:
    raise AssertionError('comparison error did not propagate')
assert len(values) == 1 and next(iter(values)) is owner
assert callbacks == [(0, 'remove_collision', 'visible comparison caller')], callbacks
callbacks.clear()
assert values.pop() is owner
assert not callbacks


class CustomSet(set):
    def pop(self):
        callbacks.append('custom pop')
        return super().pop()

    def remove(self, value):
        callbacks.append(('custom remove', value))
        return super().remove(value)


custom = CustomSet([1, 2])
assert custom.remove(1) is None
assert custom.pop() == 2
assert callbacks == [('custom remove', 1), 'custom pop']

values = {object()}
receiver_ref = weakref.ref(values)
saved_pop = values.pop
del values
gc.collect()
assert receiver_ref() is not None
returned = saved_pop()
del saved_pop
gc.collect()
assert receiver_ref() is None
assert returned is not None

lifetime = []


class Life:
    def __del__(self):
        lifetime.append('released')


values = {Life()}
returned = values.pop()
witness = weakref.ref(returned)
gc.collect()
assert witness() is returned and not lifetime
del returned
gc.collect()
assert witness() is None and lifetime == ['released']

observed = []


def traced(values):
    marker = 'traced set caller'
    values.remove(1)
    return values.pop()


def observe(frame, event, arg):
    if frame.f_code.co_name == 'traced' and event == 'return':
        observed.append((arg, frame.f_locals['marker']))
    return observe


sys.settrace(observe)
try:
    assert traced({1, 2}) == 2
finally:
    sys.settrace(None)
assert observed == [(2, 'traced set caller')], observed
print('set leaf dispatch: callbacks, owners, subclasses, and observers ok')
