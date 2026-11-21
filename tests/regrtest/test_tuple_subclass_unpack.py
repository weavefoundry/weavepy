"""Unpacking a tuple subclass's instance (a `namedtuple`'s) in compiled code.

The instance unpacks its tuple directly when its class has no Python
`__iter__`. A temporary the unpack owns sits in the first slot its items
go to, so it must move out before they are written: releasing the slot
afterwards would drop an item's reference instead of the instance's, and
an instance still reachable elsewhere would then point at a freed item.
"""

import collections

NT = collections.namedtuple("NT", "a b c")
KEEP = []


def make(i):
    t = NT([i], str(i), {"k": i})
    KEEP.append(t)
    return t


def unpack_temporaries():
    total = 0
    for i in range(3000):
        a, b, c = make(i)
        total += a[0] + len(b) + c["k"]
    return total


def unpack_held():
    point = make(7)
    total = 0
    for _ in range(3000):
        a, b, c = point
        total += a[0] + len(b) + c["k"]
    return total


class Reversed(tuple):
    def __iter__(self):
        return iter((self[2], self[1], self[0]))


def unpack_override():
    r = Reversed((1, 2, 3))
    for _ in range(3000):
        x, y, z = r
        assert (x, y, z) == (3, 2, 1), (x, y, z)


assert unpack_temporaries() == sum(2 * i + len(str(i)) for i in range(3000))
assert unpack_held() == 3000 * (7 + 1 + 7)
unpack_override()
# Every instance the loops unpacked still holds its own items.
for i, t in enumerate(KEEP[:3000]):
    assert t.a == [i] and t.b == str(i) and t.c == {"k": i}, (i, t)
try:
    a, b = make(1)
except ValueError as e:
    assert "too many values" in str(e)
else:
    raise AssertionError("expected ValueError")
