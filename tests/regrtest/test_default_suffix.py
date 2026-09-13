"""Defaults bind from the right when code has fewer positional parameters."""

import types


def template(a=10, b=20, c=30):
    return a + b + c


def scalar(value):
    return value + 1


BIAS = 1


def contextual(value):
    return value + BIAS


def pair(a, b):
    return a * 100 + b


def collect(a, /, b, *rest, flag=9, **extra):
    return a, b, rest, flag, extra


def empty():
    return 42


def make(code):
    fn = types.FunctionType(template.__code__, globals(), argdefs=(10, 20, 30))
    fn.__code__ = code
    return fn


# Check the generic binder before specialization can hide a wrong suffix.
for code, expected in ((scalar.__code__, 31), (contextual.__code__, 31),
                       (pair.__code__, 2030), (empty.__code__, 42)):
    assert make(code)() == expected
    assert make(code)(*()) == expected

assert make(pair.__code__)(b=7) == 2007
assert make(pair.__code__)(8) == 830
assert make(pair.__code__)(8, 7) == 807
collector = make(collect.__code__)
collector.__kwdefaults__ = {'flag': 9}
assert collector() == (20, 30, (), 9, {})
assert collector(4, 5, 6, flag=7, a=8) == (4, 5, (6,), 7, {'a': 8})

# Use distinct callers so both scalar-only and ordinary native frames enter.
scalar_target = make(scalar.__code__)
context_target = make(contextual.__code__)


def scalar_total(n):
    total = 0
    for _ in range(n):
        total += scalar_target()
    return total


def context_total(n):
    total = 0
    for _ in range(n):
        total += context_target()
    return total


for _ in range(60):
    assert scalar_total(50) == 1550
    assert context_total(50) == 1550
assert scalar_total(10000) == 310000
assert context_total(10000) == 310000

# Overrides invalidate native eligibility, including an oversized tuple.
scalar_target.__defaults__ = (40, 50, 60)
context_target.__defaults__ = (40, 50, 60)
assert scalar_total(50) == 3050
assert context_total(50) == 3050
scalar_target.__defaults__ = None
try:
    scalar_total(1)
except TypeError:
    pass
else:
    raise AssertionError('clearing defaults did not require the argument')
scalar_target.__defaults__ = (70,)
assert scalar_total(50) == 3550

# Bound receivers consume a positional slot before trailing defaults bind.
class Holder:
    method = make(pair.__code__)

    def __mul__(self, other):
        return other


assert Holder().method() == 130
print('default suffix semantics: ok')
