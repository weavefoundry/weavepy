"""Indexing a list or tuple by an int from compiled code keeps CPython's
results: negative indices count from the end, an index out of range raises
IndexError, items that are heap values outlive the container they came
from, a list owned by the stack (a call's result) is released, a list
sorted in place reads as empty to a key function, and a site that meets
other containers later still answers for them."""
import sys


class Box:
    def __init__(self, v):
        self.v = v


def pick(xs, i):
    return xs[i]


def walk(xs):
    total = 0
    for i in range(len(xs)):
        total += xs[i] + xs[-1 - i]
    return total


def fresh(n):
    return [Box(k) for k in range(n)]


def owned(n):
    total = 0
    for i in range(n):
        total += fresh(4)[i % 4].v
    return total


def pairs(t):
    return t[0] * t[1] - t[-1]


def run(n):
    xs = list(range(10))
    t = (3, 4, 5)
    acc = 0
    for _ in range(n):
        acc += walk(xs) + pairs(t) + pick(xs, -3) + pick(t, 1)
    return acc


assert run(3000) == 3000 * (90 + 7 + 7 + 4)
assert owned(3000) == sum(i % 4 for i in range(3000))

# A list with room to spare (built by appends), read from the end, from a
# local and through an attribute.
class Holder:
    def __init__(self):
        self.n = []
        for k in range(5):
            self.n.append(k)


def last(h):
    for _ in range(3):
        pass
    return h.n[-1], h.n[-5], h.n[4]


h = Holder()
for _ in range(3000):
    assert last(h) == (4, 0, 4)
    assert pick(h.n, -2) == 3

# A heap item outlives its list.
for _ in range(3000):
    b = pick([Box(7)], 0)
assert b.v == 7 and sys.getrefcount(b) >= 2

# Out of range, both ways.
for i in (10, -11, 1 << 40, -(1 << 40)):
    try:
        pick(list(range(10)), i)
    except IndexError as e:
        assert "list index out of range" in str(e), e
    else:
        raise AssertionError(i)
try:
    pick((1, 2), 2)
except IndexError as e:
    assert "tuple index out of range" in str(e), e
else:
    raise AssertionError("tuple")

# Inside a sort the list reads as empty.
data = [3, 1, 2]
seen = []


def key(x):
    try:
        seen.append(data[0])
    except IndexError:
        seen.append(None)
    return x


for _ in range(3000):
    pick(data, 0)
data.sort(key=key)
assert data == [1, 2, 3] and seen == [None, None, None], seen

# Stores: by an index from either end, over a scalar or a heap value, a
# borrowed or owned value, into a list read from an attribute.
def put(xs, i, v):
    xs[i] = v


def spin(h, n):
    for k in range(n):
        h.n[k % 5] = k
        h.n[-1] = h.n[0] + 1
    return h.n


h = Holder()
assert spin(h, 3000) == [2995, 2996, 2997, 2998, 2996], h.n
ys = [0] * 4
for k in range(3000):
    put(ys, k % 4, k)
    put(ys, -4, [k])
assert ys[1:] == [2997, 2998, 2999] and ys[0] == [2999], ys
for i in (4, -5):
    try:
        put(ys, i, 0)
    except IndexError as e:
        assert "list assignment index out of range" in str(e), e
    else:
        raise AssertionError(i)
put(ys, True, "b")
assert ys[1] == "b"

# A store inside a sort's key function meets an empty list.
data = [3, 1, 2]
seen = []


def store_key(x):
    try:
        put(data, 0, 9)
        seen.append("stored")
    except IndexError:
        seen.append(None)
    return x


data.sort(key=store_key)
assert data == [1, 2, 3] and seen == [None, None, None], (data, seen)

# Other containers at a site that settled on lists.
assert pick({0: "d"}, 0) == "d"
assert pick("abc", -1) == "c"
assert pick(range(5), 2) == 2
assert pick([1, 2, 3], True) == 2
print("ok")
