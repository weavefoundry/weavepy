"""Preserve argument binding and observability when entering compiled callees."""
import gc
import sys
import weakref


def add(a, b=10, c=20):
    local = a + b
    return local + c


def call_sparse(n):
    total = 0
    for i in range(n):
        total += add(i, c=5)
    return total


for unused in range(100):
    assert call_sparse(20) == 490
    assert add(c=3, a=1, b=2) == 6
    assert add(**{'c': 3, 'a': 1}) == 14

# Bind the current defaults before native entry, including a changed length.
add.__defaults__ = (100, 200)
assert call_sparse(20) == 2290
add.__defaults__ = (300,)
assert add(a=1, b=2) == 303
try:
    add(a=1)
except TypeError:
    pass
else:
    raise AssertionError('missing b did not raise')
add.__defaults__ = None
assert add(c=3, a=1, b=2) == 6
try:
    add(1, c=3)
except TypeError:
    pass
else:
    raise AssertionError('removed defaults were reused')
add.__defaults__ = (10, 20)

# An already warmed function must not keep executing a replaced code object.
saved_code = add.__code__


def replacement(a, b=10, c=20):
    return a * b + c


add.__code__ = replacement.__code__
assert add(c=5, a=3) == 35
add.__code__ = saved_code
assert add(c=5, a=3) == 18


class Counter:
    def __init__(self):
        self.total = 0

    def bump(self, by=1, factor=2):
        self.total += by * factor
        return self.total


counter = Counter()
bound = counter.bump
for i in range(100):
    assert counter.bump(factor=3) == 6 * i + 3
    assert bound(factor=3) == 6 * i + 6


def posonly(a, /, b=2):
    return a + b


for unused in range(100):
    assert posonly(1, b=4) == 5
for action in [lambda: posonly(a=1), lambda: add(1, a=2),
               lambda: add(1, missing=2), lambda: add(1, 2, 3, 4)]:
    try:
        action()
    except TypeError:
        pass
    else:
        raise AssertionError('invalid arguments were accepted')

# Layouts excluded from this entry path retain their existing binding rules.
def kwonly(a, *, b=7):
    return a + b


def variadic(a, *rest, **keywords):
    return a, rest, keywords


def outer(value):
    def closed(a, b=2):
        return value + a + b
    return closed


closed = outer(9)
for unused in range(100):
    assert kwonly(a=3) == 10
    assert variadic(1, 2, k=3) == (1, (2,), {'k': 3})
    assert closed(b=3, a=2) == 14


def generator(a, b=2):
    yield a + b


async def coroutine(a, b=2):
    return a + b


for unused in range(100):
    gen = generator(b=4, a=3)
    assert next(gen) == 7
    gen.close()
    coro = coroutine(b=4, a=3)
    try:
        coro.send(None)
    except StopIteration as stop:
        assert stop.value == 7
    else:
        raise AssertionError('coroutine did not return')

# Active observers need the callee frame and its fully bound argument values.
events = []


def trace(frame, event, arg):
    if frame.f_code is add.__code__:
        if event == 'call':
            events.append((event, frame.f_locals['a'], frame.f_locals['b'], frame.f_locals['c']))
        elif event == 'return':
            events.append((event, arg))
    return trace


sys.settrace(trace)
try:
    assert add(c=5, a=3) == 18
finally:
    sys.settrace(None)
assert events == [('call', 3, 10, 5), ('return', 18)], events
events.clear()
sys.setprofile(trace)
try:
    assert add(c=5, a=3) == 18
finally:
    sys.setprofile(None)
assert events == [('call', 3, 10, 5), ('return', 18)], events

# A native side exit must preserve the same exception and argument locals.
marker = ValueError('bound native entry')


def checked(a, b=2):
    local = a + b
    if a < 0:
        raise marker
    return local


for unused in range(100):
    assert checked(b=3, a=4) == 7
try:
    checked(b=3, a=-1)
except ValueError as error:
    assert error is marker
    tb = error.__traceback__
    while tb.tb_next is not None:
        tb = tb.tb_next
    assert tb.tb_frame.f_code is checked.__code__
    assert tb.tb_frame.f_locals['a'] == -1
    assert tb.tb_frame.f_locals['b'] == 3
    assert tb.tb_frame.f_locals['local'] == 2
else:
    raise AssertionError('native side exit lost exception')
marker.__traceback__ = None

# Release temporary arguments before the caller's next statement.
deaths = []


class Payload:
    def __init__(self, value):
        self.value = value

    def __del__(self):
        deaths.append(self.value)


def read_payload(value, offset=2):
    return value.value + offset


payload = Payload(8)
for unused in range(100):
    assert read_payload(offset=3, value=payload) == 11
reference = weakref.ref(payload)
del payload
gc.collect()
assert reference() is None
deaths.clear()
assert read_payload(offset=3, value=Payload(9)) == 12
assert deaths == [9], deaths
print('bound native entry: ok')
