"""Warm slot leaf reads preserve lookup, lifetime, and callback semantics."""
import gc
import sys
import weakref


def read(item):
    return item.left


def add(item):
    return item.left + item.right


def compare(a, b):
    return a.left < b.right


def chained(item):
    return item.left.right


def make_class(names):
    class Slots:
        __slots__ = names
    return Slots


for names in [('left',), ('left', 'right'),
              ('left', 'right') + tuple('f%d' % i for i in range(20))]:
    cls = make_class(names)
    a, b = cls(), cls()
    for name in names:
        setattr(a, name, 3 if name == 'left' else 7)
    for name in reversed(names):
        setattr(b, name, 5 if name == 'left' else 11)
    for _ in range(4000):
        assert read(a) == 3
        assert read(b) == 5
        if 'right' in names:
            assert add(a) == 10
            assert compare(a, b) is True
    del a.left
    try:
        read(a)
    except AttributeError:
        pass
    else:
        raise AssertionError('missing slot was read')
    a.left = 19
    assert read(a) == 19
    if 'right' in names:
        assert add(a) == 26
        assert compare(a, b) is False


class Base:
    __slots__ = ('left',)


class Child(Base):
    __slots__ = ('right', '__dict__', '__weakref__')


a, b = Child(), Child()
a.left, a.right = 2, 5
b.left, b.right = 7, 11
# A slot descriptor retains precedence over the instance dictionary.
a.__dict__['left'] = 99
for _ in range(4000):
    assert read(a) == 2
    assert add(a) == 7
    assert compare(a, b) is True

# Mutating the inherited descriptor invalidates a child's cached read.
events = []
saved = Base.left
class Field:
    def __get__(self, instance, owner):
        frame = sys._getframe(1)
        events.append(frame.f_code.co_name)
        if frame.f_code.co_name == 'compare':
            assert frame.f_locals['a'] is a
            assert frame.f_locals['b'] is b
        else:
            assert frame.f_locals['item'] is a
        return 13
    def __set__(self, instance, value):
        raise AssertionError('unexpected descriptor assignment')
Base.left = Field()
assert read(a) == 13
assert add(a) == 18
assert compare(a, b) is False
assert events == ['read', 'add', 'compare'], events
Base.left = saved
assert read(a) == 2

# An override introduced after warming must observe every read.
def getattribute(self, name):
    if name == 'left':
        events.append('override')
        return 17
    return object.__getattribute__(self, name)
Child.__getattribute__ = getattribute
events.clear()
assert read(a) == 17
assert add(a) == 22
assert compare(a, b) is False
assert events == ['override'] * 3, events
del Child.__getattribute__
assert read(a) == 2

# Missing-slot hooks remain on the ordinary lookup path.
def getattr_missing(self, name):
    assert name == 'left'
    events.append('missing')
    return 23
Child.__getattr__ = getattr_missing
del a.left
events.clear()
assert read(a) == 23
assert add(a) == 28
assert compare(a, b) is False
assert events == ['missing'] * 3, events
del Child.__getattr__
a.left = 2

# Chained reads keep the intermediate receiver rooted without cloning it.
a.left = b
for _ in range(4000):
    assert chained(a) == 11
b.right = 29
assert chained(a) == 29
a.left = 2

# Arbitrary arithmetic still runs with the ordinary, visible caller.
class Rich:
    def __add__(self, other):
        frame = sys._getframe(1)
        assert frame.f_code.co_name == 'add'
        assert frame.f_locals['item'] is a
        events.append(('rich-add', other))
        return 31
a.left = Rich()
events.clear()
assert add(a) == 31
assert events == [('rich-add', 5)], events

# Borrowing a returned object must retain the result exactly once.
class Payload:
    pass
payload = Payload()
a.left = payload
ref = weakref.ref(payload)
for _ in range(4000):
    assert read(a) is payload
result = read(a)
del a.left, payload
gc.collect()
assert ref() is result
del result
gc.collect()
assert ref() is None

# Active observers still see the leaf activation and populated locals.
a.left = 2
events.clear()
def trace(frame, event, arg):
    if frame.f_code is read.__code__:
        events.append(event)
        assert frame.f_locals['item'] is a
    return trace
sys.settrace(trace)
try:
    assert read(a) == 2
finally:
    sys.settrace(None)
assert 'call' in events and 'return' in events, events
refs = [weakref.ref(a), weakref.ref(b)]
del a, b
gc.collect()
assert all(ref() is None for ref in refs)
print('Pure slot reads: ok')
