"""Direct field predicates preserve value, callback, and lifetime semantics."""
import gc
import math
import sys
import weakref


class Box:
    def __init__(self, left, right):
        self.left = left
        self.right = right


class Slots:
    __slots__ = ('left', 'right')
    def __init__(self, left, right):
        self.left = left
        self.right = right


operators = [('<', lambda a, b: a < b), ('<=', lambda a, b: a <= b),
             ('==', lambda a, b: a == b), ('!=', lambda a, b: a != b),
             ('>', lambda a, b: a > b), ('>=', lambda a, b: a >= b)]
predicates = []
for symbol, operation in operators:
    for swapped in (False, True):
        namespace = {}
        operands = ('b.right', 'a.left') if swapped else ('a.left', 'b.right')
        exec('def predicate(a, b):\n    return ' + operands[0] + ' ' + symbol
             + ' ' + operands[1] + '\n', namespace)
        predicate = namespace['predicate']
        predicates.append((predicate, operation, swapped))
        a, b = Box(1, 2), Box(3, 4)
        expected = operation(4, 1) if swapped else operation(1, 4)
        for _ in range(4000):
            assert predicate(a, b) is expected

for cls in (Box, Slots):
    for a_value, b_value in [(1, 4), (4, 1), (2, 2), (-5, -2),
                             (True, False), (1.5, 1.0), (math.nan, 0),
                             (2**100, 2**101), (-(2**100), -1),
                             (2**53 - 1, float(2**53)),
                             (2**53 + 1, float(2**53)),
                             (float(2**53), 2**53 + 1),
                             (True, 1), (0, False), ('alpha', 'beta')]:
        a, b = cls(a_value, 17), cls(23, b_value)
        for predicate, operation, swapped in predicates:
            expected = (operation(b_value, a_value) if swapped
                        else operation(a_value, b_value))
            assert predicate(a, b) is expected
    same = cls(3, 5)
    for predicate, operation, swapped in predicates:
        assert predicate(same, same) is (operation(5, 3) if swapped else operation(3, 5))

# Stored cache indices must continue to name the requested fields.
a, b = Box(1, 2), Box(3, 4)
for predicate, operation, swapped in predicates:
    predicate(a, b)
    del a.left
    a.extra = 90
    a.left = 7
    del b.right
    b.extra = 80
    b.right = 9
    assert predicate(a, b) is (operation(9, 7) if swapped else operation(7, 9))

# Data descriptors invalidate warmed instance-field reads and retain order.
events = []
class Field:
    def __init__(self, name, value):
        self.name, self.value = name, value
    def __get__(self, instance, owner):
        frame = sys._getframe(1)
        events.append((self.name, frame.f_code.co_name))
        assert frame.f_locals['a'] is a
        assert frame.f_locals['b'] is b
        return self.value
    def __set__(self, instance, value):
        raise AssertionError('unexpected assignment')

Box.left = Field('left', 11)
Box.right = Field('right', 13)
for predicate, operation, swapped in predicates:
    events.clear()
    expected = operation(13, 11) if swapped else operation(11, 13)
    assert predicate(a, b) is expected
    names = ('right', 'left') if swapped else ('left', 'right')
    assert events == [(name, 'predicate') for name in names], events
del Box.left, Box.right

# Non-boolean rich results and int subclasses must stay observable.
token = object()
class RichInt(int):
    def __lt__(self, other):
        events.append(('rich-lt', other))
        return token
    def __gt__(self, other):
        events.append(('rich-gt', other))
        return token

less = predicates[0][0]
a, b = Box(RichInt(3), 0), Box(0, 5)
events.clear()
assert less(a, b) is token
assert events == [('rich-lt', 5)], events
a.left = 2
b.right = RichInt(7)
events.clear()
assert less(a, b) is token
assert events == [('rich-gt', 2)], events

# A left read that raises must prevent the right read.
class Raises:
    @property
    def left(self):
        events.append('left-raises')
        raise ValueError('field failure')
class Right:
    @property
    def right(self):
        events.append('right-read')
        return 5

events.clear()
try:
    less(Raises(), Right())
except ValueError as error:
    assert str(error) == 'field failure'
else:
    raise AssertionError('missing field failure')
assert events == ['left-raises'], events

# A left callback may mutate the field consumed by the right operand.
class Mutating:
    @property
    def left(self):
        events.append('mutate-right')
        b.right = 17
        return 3

b = Box(0, 1)
events.clear()
assert less(Mutating(), b) is True
assert events == ['mutate-right'], events
assert b.right == 17

# Default and code replacement still select the current binding and body.
def default_predicate(a, b=Box(0, 4)):
    return a.left < b.right

a = Box(3, 10)
for _ in range(4000):
    assert default_predicate(a) is True
default_predicate.__defaults__ = (Box(0, 2),)
assert default_predicate(a) is False

def other_fields(a, b):
    return a.right > b.left

saved_code = less.__code__
less.__code__ = other_fields.__code__
b = Box(12, 20)
assert less(a, b) is False
less.__code__ = saved_code
assert less(a, b) is True

# Active tracing must see the predicate activation.
a, b = Box(1, 2), Box(3, 4)
events.clear()
def trace(frame, event, arg):
    if frame.f_code is less.__code__:
        events.append(event)
    return trace
sys.settrace(trace)
try:
    assert less(a, b) is True
finally:
    sys.settrace(None)
assert 'call' in events and 'return' in events, events

refs = [weakref.ref(a), weakref.ref(b)]
del a, b
gc.collect()
assert all(ref() is None for ref in refs)
print('Pure field predicates: ok')
