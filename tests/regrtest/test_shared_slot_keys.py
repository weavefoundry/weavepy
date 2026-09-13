"""Cached slot creation preserves values, guards, deletion order, and lifetimes."""
import gc
import weakref

class Row:
    __slots__ = ('a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', '__weakref__')
    def __init__(self, value):
        self.a = value
        self.b = value
        self.c = value
        self.d = value
        self.e = value
        self.f = value
        self.g = value
        self.h = value
        self.i = value

def populate(row, value):
    row.a = value
    row.b = value
    row.c = value
    row.d = value
    row.e = value
    row.f = value
    row.g = value
    row.h = value
    row.i = value

def fields(row):
    return (row.a, row.b, row.c, row.d, row.e, row.f, row.g, row.h, row.i)

for i in range(2000):
    value = Row(i)
    assert fields(value) == (i,) * 9
    populate(value, -i)
    assert fields(value) == (-i,) * 9

left, right = Row(17), Row(23)
del left.a
del left.c
left.c = 31
left.a = 37
populate(left, 41)
assert fields(left) == (41,) * 9
assert fields(right) == (23,) * 9

# Rebinding a descriptor must invalidate the warmed store cache.
events = []
saved = Row.a
try:
    Row.a = property(lambda self: 99, lambda self, value: events.append(value))
    populate(left, 43)
    assert events == [43] and left.a == 99 and left.b == 43
finally:
    Row.a = saved
populate(left, 47)
assert fields(left) == (47,) * 9

# A changed assignment hook still sees every write.
def assign(self, name, value):
    events.append(name)
    object.__setattr__(self, name, value)
Row.__setattr__ = assign
try:
    events.clear()
    populate(left, 53)
    assert events == list('abcdefghi')
finally:
    del Row.__setattr__
assert fields(left) == (53,) * 9

class Twin:
    __slots__ = Row.__slots__
left.__class__ = Twin
populate(left, 59)
assert fields(left) == (59,) * 9

# The code can disappear while instances and their keys remain alive.
namespace = {}
exec('def write(row):\n    row.a = 61\n    row.b = 67\n', namespace)
write = namespace['write']
for _ in range(200):
    write(right)
del namespace['write']
del write
gc.collect()
assert (right.a, right.b) == (61, 67)

refs = [weakref.ref(left), weakref.ref(right)]
del left, right
gc.collect()
assert all(ref() is None for ref in refs)
print('shared slot keys: ok')
