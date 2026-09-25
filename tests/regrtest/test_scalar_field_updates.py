"""Behavioral coverage for guarded scalar field-update calls."""
import sys


class Counter:
    def __init__(self, value=0):
        self.value = value

    def advance(self, amount=2):
        self.value += amount
        return self.value

    def tick(self):
        self.value += 1
        return self.value


def invoke(counter, amount):
    return counter.advance(amount)


def invoke_default(counter):
    return counter.advance()


def warm(counter):
    total = 0
    for _ in range(4000):
        total += invoke(counter, 1)
    return total


defaults_probe = Counter()
for _ in range(4000):
    invoke_default(defaults_probe)
assert defaults_probe.value == 8000

c = Counter()
assert warm(c) == 4000 * 4001 // 2
for value, amount in [(17, -3), (-17, 3), (0, 0), (2**63 - 1, 1),
                      (-2**63, -1), (2**90, -2**85), (False, 1), (1, True)]:
    c.value = value
    assert invoke(c, amount) == value + amount
    assert c.value == value + amount
c.value = 0
for _ in range(4000):
    assert c.tick() == c.value
assert c.value == 4000

# Retained dictionaries and live iterators observe the completed value store.
d = c.__dict__
keys = iter(d)
items = iter(d.items())
assert invoke(c, 3) == 4003
assert d is c.__dict__ and d['value'] == 4003
assert list(keys) == ['value']
assert list(items) == [('value', 4003)]
del d['value']
d['padding'] = 9
try:
    invoke(c, 1)
except AttributeError:
    pass
else:
    raise AssertionError('missing field did not raise')
assert d == {'padding': 9}
d['value'] = 11
assert invoke(c, -5) == 6 and d['padding'] == 9

# Default, code, class, and instance binding changes invalidate call shortcuts.
original_defaults = Counter.advance.__defaults__
Counter.advance.__defaults__ = (9,)
assert invoke_default(c) == 15
Counter.advance.__defaults__ = original_defaults
original_code = Counter.advance.__code__
def replacement(self, amount=2):
    return -self.value
Counter.advance.__code__ = replacement.__code__
assert invoke(c, 1) == -15 and c.value == 15
Counter.advance.__code__ = original_code
original_method = Counter.advance
Counter.advance = lambda self, amount=2: -7
assert invoke(c, 1) == -7 and c.value == 15
Counter.advance = original_method
c.advance = lambda amount=2: -11
assert invoke(c, 1) == -11 and c.value == 15
del c.advance
assert invoke_default(c) == 17

# Setter and getter hooks must run inside the real callee frame.
events = []
def setter(self, name, value):
    events.append(('set', name, sys._getframe(1).f_code.co_name))
    object.__setattr__(self, name, value)
Counter.__setattr__ = setter
assert invoke(c, 1) == 18
assert events == [('set', 'value', 'advance')], events
del Counter.__setattr__
events.clear()
def getter(self, name):
    if name == 'value':
        events.append(('get', sys._getframe(1).f_code.co_name))
    return object.__getattribute__(self, name)
Counter.__getattribute__ = getter
assert invoke(c, 1) == 19
assert events == [('get', 'advance'), ('get', 'advance')], events
del Counter.__getattribute__

# A descriptor installed after warmup sees read, store, and return-read order.
events.clear()
def read_value(self):
    events.append(('read', sys._getframe(1).f_code.co_name))
    return self.__dict__['value']
def write_value(self, value):
    events.append(('write', sys._getframe(1).f_code.co_name))
    self.__dict__['value'] = value
Counter.value = property(read_value, write_value)
assert invoke(c, 1) == 20
assert events == [('read', 'advance'), ('write', 'advance'), ('read', 'advance')], events
del Counter.value

# Non-scalar arithmetic can call Python, replace values, or fail before storing.
events.clear()
class Added:
    def __iadd__(self, other):
        events.append(('iadd', other, sys._getframe(1).f_code.co_name))
        return 31
c.value = Added()
assert invoke(c, 3) == 31 and c.value == 31
assert events == [('iadd', 3, 'advance')], events
class Rejected:
    def __iadd__(self, other):
        raise ValueError('unchanged')
old = Rejected()
c.value = old
try:
    invoke(c, 3)
except ValueError as error:
    assert str(error) == 'unchanged'
else:
    raise AssertionError('arithmetic failure did not propagate')
assert c.value is old

# Slot-backed instances and stored bound-method aliases retain ordinary behavior.
class Slots:
    __slots__ = ('value',)
    advance = Counter.advance
    def __init__(self):
        self.value = 0
slot = Slots()
assert warm(slot) == 4000 * 4001 // 2
alias = c.advance
c.value = 0
assert alias(3) == 3 and c.value == 3

# Observers retain a real method call and its body events after cache warmup.
c.value = 0
warm(c)
trace_events = []
def tracer(frame, event, arg):
    if frame.f_code is Counter.advance.__code__:
        trace_events.append(event)
    return tracer
sys.settrace(tracer)
try:
    assert invoke(c, 1) == 4001
finally:
    sys.settrace(None)
assert 'call' in trace_events and 'line' in trace_events and 'return' in trace_events, trace_events
# Profiling must observe the same callee even after its caches have warmed.
profile_events = []
def profiler(frame, event, arg):
    if frame.f_code is Counter.advance.__code__:
        profile_events.append(event)
sys.setprofile(profiler)
try:
    assert invoke(c, 1) == 4002
finally:
    sys.setprofile(None)
assert profile_events == ['call', 'return'], profile_events

# Integer subclasses with arithmetic hooks must not enter the exact-int path.
class Step(int):
    def __radd__(self, other):
        assert sys._getframe(1).f_code.co_name == 'advance'
        return other + int(self) + 100
c.value = 10
assert invoke(c, Step(2)) == 112 and c.value == 112

# Replacing the whole dictionary must not reuse a stale field index.
c.__dict__ = {'padding': 71, 'value': -10}
assert invoke(c, -3) == -13
assert c.__dict__ == {'padding': 71, 'value': -13}
# Exact built-in class defaults stay shadowed by the instance dictionary.
# Replacing an admitted default with a descriptor invalidates every field cache.
class ClassDefault(Counter):
    value = 0

for default in (0, 2**90, 1.5, False, None, 'value', '\ud800', b'value', 1j,
                Counter.advance):
    ClassDefault.value = default
    d = ClassDefault()
    assert warm(d) == 4000 * 4001 // 2
    assert ClassDefault.value is default
    events.clear()
    ClassDefault.value = property(read_value, write_value)
    assert invoke(d, 1) == 4001
    assert events == [('read', 'advance'), ('write', 'advance'), ('read', 'advance')], events

# Inherited defaults and MRO changes invalidate the same cached class fact.
class BaseA:
    value = 0
class BaseB:
    value = property(read_value, write_value)
class Inherited(BaseA):
    __init__ = Counter.__init__
    advance = Counter.advance
inherited = Inherited()
assert warm(inherited) == 4000 * 4001 // 2
Inherited.__bases__ = (BaseB,)
events.clear()
assert invoke(inherited, 1) == 4001
assert events == [('read', 'advance'), ('write', 'advance'), ('read', 'advance')], events
print('scalar field update behavior: ok')
