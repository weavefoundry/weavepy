"""Keyword constructors retain binding, mutation guards, and call effects."""

import sys


class Node:
    def __init__(self, next=None, value=0):
        self.next = next
        self.value = value


def build_and_sum(n, value):
    root = Node(next=Node(value=value), value=1)
    total = 0
    for _ in range(n):
        total += root.next.value
    return total


def warm_build():
    for _ in range(100):
        assert build_and_sum(200, 3) == 600


warm_build()

# The same class object can acquire a new initializer after compilation.
original_init = Node.__init__
calls = []


def replacement_init(self, next=None, value=0):
    calls.append((next, value))
    self.next = next
    self.value = value + 1


Node.__init__ = replacement_init
assert build_and_sum(20, 3) == 80
assert len(calls) == 2
assert calls[0] == (None, 3)
assert calls[1][1] == 1
Node.__init__ = original_init
assert build_and_sum(20, 3) == 60

# Rebinding the global must honor the replacement's keyword-only signature.
OriginalNode = Node


def factory(*, next=None, value=0):
    calls.append(value)
    return OriginalNode(next, value + 2)


warm_build()
calls.clear()
Node = factory
assert build_and_sum(20, 3) == 100
assert calls == [3, 1]
Node = OriginalNode
assert build_and_sum(20, 3) == 60

# Keyword values with effects execute once, even if a callee change deopts.
effects = []


def produce(value):
    effects.append(value)
    return value


def construct(value):
    return Node(value=produce(value))


for _ in range(100):
    assert construct(7).value == 7
assert effects == [7] * 100


def fail(*, value):
    calls.append(value)
    raise ValueError('constructor failed')


Node = fail
try:
    construct(11)
except ValueError as error:
    assert str(error) == 'constructor failed'
else:
    raise AssertionError('constructor exception was lost')
assert effects == [7] * 100 + [11]
assert calls[-1] == 11
Node = OriginalNode
assert construct(13).value == 13

# Rebinding to an incompatible signature must still raise Python's TypeError.
def positional_only(value, /):
    raise AssertionError('invalid keyword binding executed the body')


warm_build()
Node = positional_only
try:
    build_and_sum(1, 3)
except TypeError:
    pass
else:
    raise AssertionError('positional-only binding accepted a keyword')
Node = OriginalNode

# A constructor callback sees its caller's current locals and instruction.
frames = []


def inspected_factory(*, value):
    frame = sys._getframe(1)
    frames.append((frame.f_code.co_name, frame.f_locals['value']))
    return OriginalNode(value=value)


Node = inspected_factory
assert construct(17).value == 17
assert frames == [('construct', 17)]
Node = OriginalNode
warm_build()
assert build_and_sum(20, 1.25) == 25.0
assert build_and_sum(20, 2**80) == 20 * 2**80
# Builtin callees demoted by the same rule must retain argument errors.
def measure_length(value, invalid):
    if invalid:
        return len(obj=value)
    return len(value)


for _ in range(100):
    assert measure_length([1, 2, 3], False) == 3
try:
    measure_length([1, 2, 3], True)
except TypeError:
    pass
else:
    raise AssertionError('len accepted a keyword argument')
print('keyword constructors: ok')
