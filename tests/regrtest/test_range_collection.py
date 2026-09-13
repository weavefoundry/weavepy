"""Exact range collection, boundary integers, and generic iterator behavior."""
import gc
import weakref

# Values recorded from CPython, including ranges whose bounds exceed i128.
CASES = [
    ((0,), []),
    ((1,), [0]),
    ((257,), [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49, 50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 63, 64, 65, 66, 67, 68, 69, 70, 71, 72, 73, 74, 75, 76, 77, 78, 79, 80, 81, 82, 83, 84, 85, 86, 87, 88, 89, 90, 91, 92, 93, 94, 95, 96, 97, 98, 99, 100, 101, 102, 103, 104, 105, 106, 107, 108, 109, 110, 111, 112, 113, 114, 115, 116, 117, 118, 119, 120, 121, 122, 123, 124, 125, 126, 127, 128, 129, 130, 131, 132, 133, 134, 135, 136, 137, 138, 139, 140, 141, 142, 143, 144, 145, 146, 147, 148, 149, 150, 151, 152, 153, 154, 155, 156, 157, 158, 159, 160, 161, 162, 163, 164, 165, 166, 167, 168, 169, 170, 171, 172, 173, 174, 175, 176, 177, 178, 179, 180, 181, 182, 183, 184, 185, 186, 187, 188, 189, 190, 191, 192, 193, 194, 195, 196, 197, 198, 199, 200, 201, 202, 203, 204, 205, 206, 207, 208, 209, 210, 211, 212, 213, 214, 215, 216, 217, 218, 219, 220, 221, 222, 223, 224, 225, 226, 227, 228, 229, 230, 231, 232, 233, 234, 235, 236, 237, 238, 239, 240, 241, 242, 243, 244, 245, 246, 247, 248, 249, 250, 251, 252, 253, 254, 255, 256]),
    ((0, 11, 3), [0, 3, 6, 9]),
    ((11, -1, -3), [11, 8, 5, 2]),
    ((9, 2, 3), []),
    ((2, 9, -3), []),
    ((-15, 16, 7), [-15, -8, -1, 6, 13]),
    ((15, -16, -7), [15, 8, 1, -6, -13]),
    ((9223372036854775804, 9223372036854775807), [9223372036854775804, 9223372036854775805, 9223372036854775806]),
    ((-9223372036854775808, -9223372036854775804), [-9223372036854775808, -9223372036854775807, -9223372036854775806, -9223372036854775805]),
    ((9223372036854775807, -9223372036854775808, -9223372036854775808), [9223372036854775807, -1]),
    ((-9223372036854775808, 9223372036854775807, 9223372036854775807), [-9223372036854775808, -1, 9223372036854775806]),
    ((9223372036854775807, 9223372036854775811), [9223372036854775807, 9223372036854775808, 9223372036854775809, 9223372036854775810]),
    ((-9223372036854775811, -9223372036854775807), [-9223372036854775811, -9223372036854775810, -9223372036854775809, -9223372036854775808]),
    ((1267650600228229401496703205376, 1267650600228229401496703205386, 3), [1267650600228229401496703205376, 1267650600228229401496703205379, 1267650600228229401496703205382, 1267650600228229401496703205385]),
    ((-1267650600228229401496703205376, -1267650600228229401496703205386, -3), [-1267650600228229401496703205376, -1267650600228229401496703205379, -1267650600228229401496703205382, -1267650600228229401496703205385]),
    ((1606938044258990275541962092341162602522202993782792835301376, 1606938044258990275541962092341162602522202993782792835301386, 3), [1606938044258990275541962092341162602522202993782792835301376, 1606938044258990275541962092341162602522202993782792835301379, 1606938044258990275541962092341162602522202993782792835301382, 1606938044258990275541962092341162602522202993782792835301385]),
    ((-1606938044258990275541962092341162602522202993782792835301376, -1606938044258990275541962092341162602522202993782792835301386, -3), [-1606938044258990275541962092341162602522202993782792835301376, -1606938044258990275541962092341162602522202993782792835301379, -1606938044258990275541962092341162602522202993782792835301382, -1606938044258990275541962092341162602522202993782792835301385]),
]


class ListSubclass(list):
    pass


class TupleSubclass(tuple):
    pass


def check(bounds, expected):
    value = range(*bounds)
    result = list(value)
    assert result == expected and gc.is_tracked(result)
    result = tuple(value)
    assert list(result) == expected and tuple(result) is result
    assert list(tuple.__new__(tuple, value)) == expected
    assert ListSubclass(value) == expected
    assert list(TupleSubclass(value)) == expected
    initialized = []
    list.__init__(initialized, value)
    assert initialized == expected
    assert [*value] == expected
    assert sorted(value) == sorted(expected)
    iterator = iter(value)
    next(iterator, None)
    assert list(iterator) == expected[1:]
    assert list(iterator) == []


for bounds, expected in CASES:
    for _ in range(12):
        check(bounds, expected)

# A wrapper's iteration remains observable and may yield different values
# from any range stored on it. List construction still invokes its length hook.
class Source:
    def __init__(self):
        self.value = range(3)
        self.iterations = 0
        self.lengths = 0

    def __len__(self):
        self.lengths += 1
        return 3

    def __iter__(self):
        self.iterations += 1
        return iter((8, 5, 2))


for constructor in (list, tuple):
    source = Source()
    assert list(constructor(source)) == [8, 5, 2]
    assert source.iterations == 1
    if constructor is list:
        assert source.lengths == 1
    # The preceding release also probes Source.__len__ for tuple(Source()).
    # CPython probes the returned iterator instead. The separate protocol
    # report retains that existing difference; this optimization targets Range.

# Range construction retains index callbacks, including their order.
events = []


class Index:
    def __init__(self, value):
        self.value = value

    def __index__(self):
        events.append(self.value)
        return self.value


assert list(range(Index(2), Index(12), Index(3))) == [2, 5, 8, 11]
assert events == [2, 12, 3]

# Lists remain tracked after the direct fill so later cycles are reclaimed.
class Node:
    pass


def cycle():
    result = list(range(10))
    node = Node()
    node.back = result
    result.append(node)
    return weakref.ref(node)


reference = cycle()
gc.collect()
assert reference() is None
print('ok')
