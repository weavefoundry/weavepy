"""Small native numeric calls preserve binding, errors, and observers."""

import sys
import types


def leaf_add(a, b):
    return a + b


def leaf_default(a, b=10, c=20):
    return a + b + c


def leaf_divide(a, b):
    return (a + 0) / (b + 0)


def leaf_float(a, b):
    return (a + 0.0) / (b + 0.0)


def leaf_sum(n, callback):
    total = 0
    for i in range(n):
        total += callback(i, 1)
    return total


def leaf_defaults(n):
    total = 0
    for i in range(n):
        total += leaf_default(i)
    return total


for _ in range(60):
    assert leaf_sum(50, leaf_add) == 1275
    assert leaf_defaults(50) == 2725
    assert leaf_divide(9, 2) == 4.5
    assert leaf_float(9.0, 2.0) == 4.5
assert leaf_sum(100000, leaf_add) == 5000050000
assert leaf_add(2**63 - 1, 1) == 2**63
assert leaf_add(-(2**63), -1) == -(2**63) - 1
assert leaf_add(2**100, 7) == 2**100 + 7
assert leaf_add(True, 1) == 2
assert leaf_add(0.5, 2) == 2.5
assert leaf_divide(2**60 + 123, 7).hex() == '0x1.2492492492493p+57'


def call_divide(callback, a, b):
    return callback(a, b)


for callback, a, b in ((leaf_divide, 1, 0), (leaf_float, 1.0, 0.0)):
    try:
        call_divide(callback, a, b)
    except ZeroDivisionError as error:
        names = []
        tb = error.__traceback__
        while tb is not None:
            names.append(tb.tb_frame.f_code.co_name)
            tb = tb.tb_next
        assert names[-2:] == ['call_divide', callback.__name__], names
    else:
        raise AssertionError('division by zero did not raise')

# A native caller must recover from a callee overflow, too.
assert leaf_sum(4, lambda a, b: leaf_add(2**63 - 1, b)) == 4 * 2**63
leaf_default.__defaults__ = (3, 4)
assert leaf_defaults(50) == 1575
assert leaf_default(2, c=9) == 14
leaf_default.__defaults__ = None
try:
    leaf_defaults(1)
except TypeError:
    pass
else:
    raise AssertionError('missing defaults did not raise')
leaf_default.__defaults__ = (10, 20)
assert leaf_defaults(50) == 2725


def subtract(a, b):
    return a - b


old = leaf_add.__code__
leaf_add.__code__ = subtract.__code__
assert leaf_sum(50, leaf_add) == 1175
leaf_add.__code__ = old
assert leaf_sum(50, leaf_add) == 1275

seen = []


class Number(int):
    def __add__(self, other):
        seen.append((int(self), other))
        return int(self) + other + 100


assert leaf_add(Number(2), 3) == 105
assert seen == [(2, 3)]

# Shared code can still read different function namespaces. Such functions
# need the general context and its namespace guards.
def global_value(a, b):
    return a + b + OFFSET


one = types.FunctionType(global_value.__code__, {'OFFSET': 1})
two = types.FunctionType(global_value.__code__, {'OFFSET': 2})
for _ in range(60):
    assert leaf_sum(20, one) == 230
    assert leaf_sum(20, two) == 250

# Tracing a previously compiled caller must expose every callee event.
seen.clear()


def trace(frame, event, arg):
    if frame.f_code is leaf_add.__code__ and event in ('call', 'return'):
        seen.append(event)
    return trace


sys.settrace(trace)
try:
    assert leaf_sum(5, leaf_add) == 15
finally:
    sys.settrace(None)
assert seen == ['call', 'return'] * 5, seen


def recursive(depth):
    if depth == 0:
        return leaf_add(1, 2)
    return recursive(depth - 1) + 0


for _ in range(60):
    assert recursive(10) == 3
old_limit = sys.getrecursionlimit()
sys.setrecursionlimit(150)
try:
    try:
        recursive(500)
    except RecursionError:
        pass
    else:
        raise AssertionError('native recursion bypassed the limit')
finally:
    sys.setrecursionlimit(old_limit)
assert leaf_sum(50, leaf_add) == 1275


def leaf_branch(a, b):
    if a < 0:
        return a - b
    return a + b


def leaf_constant(a, b):
    return 7


def leaf_one(n, callback):
    total = 0
    for i in range(n):
        total += callback(i)
    return total


# Non-leaf frames, unused parameters, namespace guards, and missing trailing
# arguments reuse ordinary call resolution after the borrowed path declines.
assert leaf_sum(4000, leaf_branch) == 8002000
assert leaf_branch(-1, 2) == -3
assert leaf_sum(4000, leaf_constant) == 28000
assert leaf_one(4000, leaf_default) == 8118000
leaf_default.__defaults__ = (3, 4)
assert leaf_one(4000, leaf_default) == 8026000
leaf_default.__defaults__ = None
try:
    leaf_one(1, leaf_default)
except TypeError:
    pass
else:
    raise AssertionError('dynamic call ignored missing defaults')
leaf_default.__defaults__ = (10, 20)
assert leaf_one(50, leaf_default) == 2725
one.__globals__['OFFSET'] = 7
assert leaf_sum(50, one) == 1625
one.__globals__['OFFSET'] = 1
assert leaf_sum(50, one) == 1325
print('ok')
