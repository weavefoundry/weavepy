"""Conditional field getters read exactly the selected field in order."""
import gc
import sys
import weakref


class Config:
    FORWARD = 1


class Item:
    def __init__(self, direction, left, right):
        self.direction, self.left, self.right = direction, left, right


def selected(item):
    if item.direction == Config.FORWARD:
        return item.left
    return item.right


def reversed_selected(item):
    if item.direction == Config.FORWARD:
        return item.right
    return item.left


def warm_branches(item):
    for _ in range(4000):
        assert selected(item) is item.left
        assert reversed_selected(item) is item.right
        item.direction = 2
        assert selected(item) is item.right
        assert reversed_selected(item) is item.left
        item.direction = 1


def repeat_choice(choice, item, expected):
    for _ in range(1000):
        assert choice(item) is expected


a = Item(1, object(), object())
warm_branches(a)

# Scalar comparisons share ordinary Python boundary and NaN behavior.
for actual, expected in [(1.0, 1), (True, 1), (2**53 + 1, float(2**53)),
                         (float('nan'), 1), (None, None), ('one', 'one'),
                         (2**100, 2**100)]:
    a.direction = actual
    Config.FORWARD = expected
    assert selected(a) is (a.left if actual == expected else a.right)
Config.FORWARD = 1
a.direction = 1

# A warmed scalar constant can become a string and become scalar again.
for expected in [1, 'one', True, 'two', 1]:
    Config.FORWARD = expected
    a.direction = expected
    repeat_choice(selected, a, a.left)
    a.direction = 'different' if isinstance(expected, str) else 2
    repeat_choice(selected, a, a.right)
Config.FORWARD = 1
a.direction = 1

# All comparison operators and both branch result orders are preserved.
for symbol, operation in [('<', lambda x, y: x < y), ('<=', lambda x, y: x <= y),
                          ('==', lambda x, y: x == y), ('!=', lambda x, y: x != y),
                          ('>', lambda x, y: x > y), ('>=', lambda x, y: x >= y)]:
    namespace = {'Config': Config}
    exec('def choose(item):\n    if item.direction ' + symbol
         + ' Config.FORWARD:\n        return item.left\n    return item.right\n', namespace)
    choose = namespace['choose']
    for value in (0, 1, 2):
        a.direction = value
        expected = a.left if operation(value, 1) else a.right
        repeat_choice(choose, a, expected)
a.direction = 1

# The unchosen branch must not read a missing field or its descriptor.
events = []
class Right:
    def __get__(self, instance, owner):
        events.append('right-read')
        raise ValueError('right field')
    def __set__(self, instance, value):
        raise AssertionError('unexpected assignment')
Item.right = Right()
assert selected(a) is a.left
assert events == []
a.direction = 2
try:
    selected(a)
except ValueError as error:
    assert str(error) == 'right field'
else:
    raise AssertionError('selected descriptor was skipped')
assert events == ['right-read']
del Item.right
assert selected(a) is a.right

# A condition callback runs first and can change the selected value.
class Direction:
    def __get__(self, instance, owner):
        frame = sys._getframe(1)
        assert frame.f_code.co_name == 'selected'
        assert frame.f_locals['item'] is a
        events.append('direction')
        a.left = 'updated'
        return 1
    def __set__(self, instance, value):
        raise AssertionError('unexpected assignment')
Item.direction = Direction()
events.clear()
assert selected(a) == 'updated'
assert events == ['direction']
del Item.direction

# Class constants and the global class binding are mutable.
a.direction = 1
Config.FORWARD = 2
assert selected(a) is a.right
Config.FORWARD = 1
assert selected(a) is a.left
old_config = Config
class Config:
    FORWARD = 9
assert selected(a) is a.right
Config = old_config
assert selected(a) is a.left

# A nondefault metaclass may intercept the comparison constant.
class Meta(type):
    def __getattribute__(cls, name):
        if name == 'FORWARD':
            frame = sys._getframe(1)
            assert frame.f_code.co_name == 'selected'
            assert frame.f_locals['item'] is a
            events.append('metaclass')
            return 2
        return type.__getattribute__(cls, name)
class MetaConfig(metaclass=Meta):
    FORWARD = 1
Config = MetaConfig
events.clear()
assert selected(a) is a.right
assert events == ['metaclass'], events
Config = old_config
assert selected(a) is a.left

# A class value becoming a descriptor must invalidate the borrowed answer.
class ConstantDescriptor:
    def __get__(self, instance, owner):
        frame = sys._getframe(1)
        assert frame.f_code.co_name == 'selected'
        assert instance is None and owner is Config
        events.append('constant-descriptor')
        return 1

repeat_choice(selected, a, a.left)
Config.FORWARD = ConstantDescriptor()
events.clear()
assert selected(a) is a.left
assert events == ['constant-descriptor'], events
del Config.FORWARD
try:
    selected(a)
except AttributeError:
    pass
else:
    raise AssertionError('missing class constant must raise')
Config.FORWARD = 1
repeat_choice(selected, a, a.left)

# Defaults and code replacement continue to use the current binding.
def default_selected(item=Item(1, 'default-left', 'default-right')):
    if item.direction == Config.FORWARD:
        return item.left
    return item.right
for _ in range(4000):
    assert default_selected() == 'default-left'
default_selected.__defaults__ = (Item(2, 'new-left', 'new-right'),)
assert default_selected() == 'new-right'
saved_code = default_selected.__code__
default_selected.__code__ = reversed_selected.__code__
assert default_selected() == 'new-left'
default_selected.__code__ = saved_code
assert default_selected() == 'new-right'

# Rich comparison and truth callbacks keep their order and caller frame.
class Truth:
    def __bool__(self):
        frame = sys._getframe(1)
        assert frame.f_code.co_name == 'selected'
        events.append('truth')
        a.right = 'truth-selected'
        return False
class DirectionValue:
    def __eq__(self, other):
        frame = sys._getframe(1)
        assert frame.f_code.co_name == 'selected'
        assert frame.f_locals['item'] is a
        events.append(('compare', other))
        return Truth()
a.direction = DirectionValue()
events.clear()
assert selected(a) == 'truth-selected'
assert events == [('compare', 1), 'truth'], events

# A missing unchosen field does not require both result caches to hit.
a.direction = 1
del a.right
assert selected(a) is a.left
a.direction = 2
a.right = 'right-restored'
del a.left
assert selected(a) is a.right

# Returned values outlive receivers; unused objects are collectible.
class Payload:
    pass
payload = Payload()
a.left = payload
a.direction = 1
for _ in range(4000):
    assert selected(a) is payload
result = selected(a)
refs = [weakref.ref(a), weakref.ref(payload)]
del a, payload
gc.collect()
assert refs[0]() is None
assert refs[1]() is result
del result
gc.collect()
assert refs[1]() is None

# Observers see the ordinary activation.
a = Item(1, 3, 5)
events.clear()
def trace(frame, event, arg):
    if frame.f_code is selected.__code__:
        assert frame.f_locals['item'] is a
        events.append(event)
    return trace
sys.settrace(trace)
try:
    assert selected(a) == 3
finally:
    sys.settrace(None)
assert 'call' in events and 'return' in events, events
print('Conditional field getters: ok')
