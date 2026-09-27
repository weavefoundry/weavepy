"""Literal call scratch preserves argument values, fallback, and observers."""
import sys


def add(value, amount=3):
    return value + amount


class Rules:
    @staticmethod
    def add(value, amount=3):
        return value + amount

    def method(self, value, amount=3):
        return value + amount


def calls(value, rules):
    return (add(value, 3), Rules.add(3, value), rules.method(value, 3),
            add(7), Rules.add(7), rules.method(7))


rules = Rules()
for value in range(4000):
    assert calls(value, rules) == (value + 3, value + 3, value + 3, 10, 10, 10)

# Each scratch position must hold its own value. Bound calls reserve position
# zero for the receiver; the next explicit argument exceeds the eight slots.
names = list('abcdefgh')
for position in range(8):
    scope = {}
    exec('def pick(' + ', '.join(names) + '):\n    return ' + names[position], scope)
    exec('def caller():\n    return pick(0, 1, 2, 3, 4, 5, 6, 7)', scope)
    for _ in range(300):
        assert scope['caller']() == position
for count in (7, 8, 9):
    scope = {}
    args = ['x' + str(i) for i in range(count)]
    exec('class Bound:\n    def pick(self, ' + ', '.join(args) +
         '):\n        return ' + args[-1], scope)
    exec('def caller(obj):\n    return obj.pick(' +
         ', '.join(str(i) for i in range(count)) + ')', scope)
    obj = scope['Bound']()
    for _ in range(300):
        assert scope['caller'](obj) == count - 1


def take8(a, b, c, d, e, f, g, h):
    return h


def mixed(value):
    return take8(0, value, -1, None, 'constant', 2.5, 65536, value)


def zero():
    return 4


def no_args():
    return zero()


token = object()
for _ in range(4000):
    assert mixed(token) is token
    assert no_args() == 4

# A later operand can force fallback after the scratch contains a literal.
events = []
class Source:
    @property
    def value(self):
        frame = sys._getframe(1)
        assert frame.f_code.co_name == 'with_field'
        assert frame.f_locals['source'] is source
        events.append('field')
        return 5


def with_field(source):
    return add(3, source.value)


source = Source()
for _ in range(300):
    assert with_field(source) == 8
assert events == ['field'] * 300


def later():
    events.append('later')
    return 5


def with_callback():
    return add(3, later())


events.clear()
for _ in range(300):
    assert with_callback() == 8
assert events == ['later'] * 300


def raises():
    events.append('raise')
    raise ValueError('argument')


def failed_argument():
    return add(3, raises())


events.clear()
try:
    failed_argument()
except ValueError as error:
    assert str(error) == 'argument'
else:
    raise AssertionError('later argument must raise')
assert events == ['raise']

# Warm call slots cannot retain old code, defaults, or instance bindings.
def subtract(value, amount=3):
    return value - amount


saved_code = add.__code__
add.__code__ = subtract.__code__
assert calls(20, rules) == (17, 23, 23, 4, 10, 10)
add.__code__ = saved_code
add.__defaults__ = (9,)
assert calls(20, rules) == (23, 23, 23, 16, 10, 10)
add.__defaults__ = (3,)
rules.method = lambda value, amount=3: value - amount
assert calls(20, rules) == (23, 23, 17, 10, 10, 4)
del rules.method
assert calls(20, rules) == (23, 23, 23, 10, 10, 10)

# A failed pure operation must restore ordinary Python callback frames.
class Rich:
    def __radd__(self, other):
        frame = sys._getframe(1)
        assert frame.f_code.co_name == 'add'
        assert frame.f_locals['value'] == 3
        assert frame.f_locals['amount'] is rich
        return token


def rich_call(value):
    return add(3, value)


for _ in range(4000):
    assert rich_call(5) == 8
rich = Rich()
assert rich_call(rich) is token

events.clear()
def trace(frame, event, arg):
    if frame.f_code is add.__code__:
        assert frame.f_locals['value'] == 3
        assert frame.f_locals['amount'] == 5
        events.append(event)
    return trace


sys.settrace(trace)
try:
    assert rich_call(5) == 8
finally:
    sys.settrace(None)
assert 'call' in events and 'return' in events, events
print('Pure literal arguments: ok')
