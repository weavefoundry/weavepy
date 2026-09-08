"""Recycled call metadata must release owners and preserve escaped frames."""
import gc
import sys
import weakref


def identity(value):
    return value


# Exercise immediate and shared object copies at repeatedly reused call sites.
for value in (None, True, False, -(2**63), 2**63 - 1, 2**100, -0.0,
              float('inf'), 'unicode \u03bb', b'bytes', [1], {'a': 2}, (3,)):
    for _ in range(50):
        result = identity(value)
        assert result is value


def capture(value):
    cell = value
    def closure():
        return cell
    return sys._getframe(), closure


frame, closure = capture('retained')
for i in range(200):
    assert identity(i) == i
assert frame.f_code.co_name == 'capture'
assert frame.f_locals['value'] == 'retained'
assert closure() == 'retained'
assert frame.f_back is sys._getframe()


class Token:
    pass


def ephemeral():
    token = Token()
    return weakref.ref(token)


for _ in range(200):
    assert ephemeral()() is None


# A completed call must not keep a dynamically created globals mapping alive.
def isolated():
    token = Token()
    ref = weakref.ref(token)
    namespace = {'token': token}
    exec('def invoke():\n    return token\n', namespace)
    invoke = namespace.pop('invoke')
    assert invoke() is token
    return ref


ref = isolated()
gc.collect()
assert ref() is None


# Parked generator shells remain live, with stable frame identity.
def values():
    for i in range(3):
        yield i


generator = values()
generator_frame = generator.gi_frame
for i in range(3):
    assert next(generator) == i
    assert generator.gi_frame is generator_frame
    assert generator_frame.f_back is None
    assert identity(i) == i
try:
    next(generator)
except StopIteration:
    pass
else:
    raise AssertionError('generator did not finish')
assert generator.gi_frame is None

# Cached imports must observe direct sys.modules replacement and deletion.
import importlib
from types import ModuleType

name = '_weavepy_cache_\u03bb'
first = ModuleType(name)
second = ModuleType(name)
try:
    sys.modules[name] = first
    assert importlib.import_module(name) is first
    sys.modules[name] = second
    assert importlib.import_module(name) is second
    del sys.modules[name]
    try:
        importlib.import_module(name)
    except ModuleNotFoundError:
        pass
    else:
        raise AssertionError('removed module stayed cached')
finally:
    sys.modules.pop(name, None)
