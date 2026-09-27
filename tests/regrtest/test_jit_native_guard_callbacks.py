# Native namespace guards must release the JIT cache before calling Python.
import types


def template(a, b):
    return a + b + OFFSET


def drive(n, callback):
    total = 0
    for i in range(n):
        total += callback(i, 1)
    return total


def nested_sum(n):
    total = 0
    for i in range(n):
        total += i
    return total


namespace = {'OFFSET': 1}
callback = types.FunctionType(template.__code__, namespace)
assert drive(4000, callback) == 8006000
calls = 0


class OffsetKey:
    def __hash__(self):
        return hash('OFFSET')

    def __eq__(self, other):
        global calls
        calls += 1
        assert nested_sum(4000) == 7998000
        return other == 'OFFSET'


del namespace['OFFSET']
namespace[OffsetKey()] = 1
assert drive(3, callback) == 9
assert calls > 0, calls
namespace['OFFSET'] = 2
assert drive(3, callback) == 12
print('Native namespace guard callbacks: ok')
