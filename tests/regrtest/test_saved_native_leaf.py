"""Saved native methods preserve values, owners, and observer transitions."""
import gc
import sys
import weakref


def call_zero(callback):
    return callback()


for callback, expected in [([2, 3].__len__, 2), ([5, 7].copy, [5, 7]),
                           ({'a': 11}.__len__, 1), ({13}.__len__, 1),
                           ('a\u03b2'.__len__, 2), ('aB'.upper, 'AB'),
                           (' Ab '.strip, 'Ab'), ('a b'.split, ['a', 'b'])]:
    for unused in range(100):
        assert call_zero(callback) == expected

# An inherited native body has a different admitted receiver shape. A miss
# for the subclass must not disable the same body on an exact receiver.
class Child(list):
    pass


ordinary = Child([17, 19])
inherited = ordinary.copy
exact = [23, 29].copy
for unused in range(100):
    assert call_zero(inherited) == [17, 19]
    assert call_zero(exact) == [23, 29]


class Override(list):
    def __len__(self):
        return 31


overridden = Override().__len__
for unused in range(100):
    assert call_zero(overridden) == 31
    assert call_zero(exact) == [23, 29]

items = [1, 2, 3]
reverse = items.reverse
for unused in range(100):
    assert call_zero(reverse) is None
    assert items == ([3, 2, 1] if unused % 2 == 0 else [1, 2, 3])

mapping = {'first': 2, 'second': 3}
for callback, expected in [(mapping.keys, ['first', 'second']),
                           (mapping.values, [2, 3]),
                           (mapping.items, [('first', 2), ('second', 3)])]:
    for unused in range(100):
        assert list(call_zero(callback)) == expected

# Neither admitted nor rejected method caches may keep the saved method
# alive. The native operation and receiver remain callable while it's saved.
for operation in ('copy', 'clear'):
    saved = getattr([], operation)
    witness = weakref.ref(saved)
    for unused in range(100):
        result = call_zero(saved)
        assert result == ([] if operation == 'copy' else None)
    del saved
    gc.collect()
    assert witness() is None, operation

# Errors and argument-bearing calls keep their normal dispatch behavior.
callback = [].append
for unused in range(100):
    try:
        call_zero(callback)
    except TypeError:
        pass
    else:
        raise AssertionError('append accepted a missing value')
callback(37)
assert callback.__self__ == [37]

# A callable held only by the operand stack must retain ordinary prompt
# receiver teardown. Such calls can't use the saved-owner shortcut.
deaths = []


class Payload:
    def __del__(self):
        deaths.append(1)


def temporary_method():
    return [Payload()].reverse


for unused in range(100):
    assert temporary_method()() is None
    assert len(deaths) == unused + 1

# Hooks installed after warming must still see native calls and Python
# frames. Removing them must let the same saved method keep working.
callback = 'Ab'.upper
for unused in range(100):
    assert call_zero(callback) == 'AB'
events = []


def profile(frame, event, arg):
    if event in ('c_call', 'c_return', 'c_exception') and arg == callback:
        events.append(event)


sys.setprofile(profile)
try:
    assert call_zero(callback) == 'AB'
finally:
    sys.setprofile(None)
assert events == ['c_call', 'c_return'], events
events.clear()


def trace(frame, event, arg):
    if frame.f_code is call_zero.__code__:
        if event == 'call':
            assert frame.f_locals['callback'] is callback
            events.append(event)
        elif event == 'return':
            events.append((event, arg))
    return trace


sys.settrace(trace)
try:
    assert call_zero(callback) == 'AB'
finally:
    sys.settrace(None)
assert events == ['call', ('return', 'AB')], events
assert call_zero(callback) == 'AB'
print('saved native methods preserve owners and observers')
