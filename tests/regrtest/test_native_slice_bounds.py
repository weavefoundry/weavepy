"""Keep extreme, omitted, and fallback native slice bounds exact."""
MIN = -9223372036854775808
MAX = 9223372036854775807


def list_stop(items, stop):
    return items[:stop:None]


def str_stop(items, stop):
    return items[:stop:None]


def list_pair(items, start, stop):
    return items[start:stop:None]


def str_pair(items, start, stop):
    return items[start:stop:None]


def list_open(items, start):
    return items[start::None]


def str_open(items, start):
    return items[start::None]


bounds = [MIN, MIN + 1, -100, -3, -1, 0, 1, 3, 100, MAX]
for repeat in range(100):
    for items, stop_function, pair_function, open_function in [
        ([1, 2, 3], list_stop, list_pair, list_open),
        ('abc', str_stop, str_pair, str_open),
    ]:
        for stop in bounds:
            assert stop_function(items, stop) == items[slice(None, stop)]
            assert pair_function(items, -1, stop) == items[slice(-1, stop)]
            assert pair_function(items, MIN, stop) == items[slice(MIN, stop)]
            assert open_function(items, stop) == items[slice(stop, None)]

# Switching from warmed native inputs must retain generic behavior.
for items, stop_function, pair_function, open_function in [
    ([], list_stop, list_pair, list_open),
    ([None, object(), 'tail'], list_stop, list_pair, list_open),
    ('\u00e9\U0001f600z', str_stop, str_pair, str_open),
    ('\ud800z', str_stop, str_pair, str_open),
]:
    for stop in [MIN, MAX, -(1 << 100), 1 << 100, None]:
        assert stop_function(items, stop) == items[slice(None, stop)]
        assert pair_function(items, 1, stop) == items[slice(1, stop)]
        assert open_function(items, stop) == items[slice(stop, None)]


class Bound:
    def __init__(self, value):
        self.value = value
        self.calls = 0

    def __index__(self):
        self.calls += 1
        return self.value


for items, function in [([1, 2, 3], list_stop), ('abc', str_stop)]:
    stop = Bound(MIN)
    assert function(items, stop) == items[:0]
    assert stop.calls == 1

# A two-bound producer must restore exactly two bounds on native fallback.
import dis
import opcode


def two_bounds(value, stop):
    return value[:stop:None]


instructions = list(dis.get_instructions(two_bounds))
index = next(i for i, instruction in enumerate(instructions)
             if instruction.opname == 'BUILD_SLICE')
build = instructions[index]
step = instructions[index - 1]
assert build.arg == 3 and step.opname == 'LOAD_CONST' and step.argval is None
wire = bytearray(two_bounds.__code__.co_code)
wire[step.offset] = opcode.opmap['NOP']
wire[step.offset + 1] = 0
wire[build.offset + 1] = 2
two_bounds.__code__ = two_bounds.__code__.replace(co_code=bytes(wire))
for _ in range(100):
    assert two_bounds('abcdef', 4) == 'abcd'
assert two_bounds('é😀z', 2) == 'é😀'
assert two_bounds('é😀z', MIN) == ''

print('native slice bounds: ok')
