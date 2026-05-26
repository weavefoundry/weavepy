# RFC 0027 Group 4: Containers + array.array.
#
# Bundles CPython 3.13 conformance fixtures for the parts of the
# regrtest sweep that exercise dict / set / frozenset, the
# `collections` family (OrderedDict, defaultdict, Counter, ChainMap,
# deque), `heapq`, `bisect`, and the binary `array.array` type. The
# goal is *behavioral* parity: ordering, identity, mutation under
# iteration, default-factory semantics, key= callables, and the
# `array.array` typed buffer.
#
# Each block is self-contained; the file fails fast on the first
# divergence so the location of the gap is easy to spot.


# ---------- dict insertion order (PEP 468) ----------
d = {}
d["b"] = 2
d["a"] = 1
d["c"] = 3
assert list(d) == ["b", "a", "c"]
assert list(d.keys()) == ["b", "a", "c"]
assert list(d.values()) == [2, 1, 3]
assert list(d.items()) == [("b", 2), ("a", 1), ("c", 3)]

d2 = dict(d)
assert list(d2) == ["b", "a", "c"]

d3 = {k: v for k, v in d.items()}
assert list(d3) == ["b", "a", "c"]

del d["a"]
d["a"] = 9
assert list(d) == ["b", "c", "a"], list(d)


# ---------- dict.pop / popitem ----------
d = {"x": 1, "y": 2, "z": 3}
assert d.pop("y") == 2
assert d == {"x": 1, "z": 3}
assert d.popitem() == ("z", 3)
assert d == {"x": 1}


# ---------- dict.fromkeys ----------
assert dict.fromkeys(["a", "b", "c"], 0) == {"a": 0, "b": 0, "c": 0}
assert list(dict.fromkeys("abc", None)) == ["a", "b", "c"]


# ---------- set / frozenset basics ----------
s = {1, 2, 3}
assert s | {3, 4} == {1, 2, 3, 4}
assert s & {2, 3, 9} == {2, 3}
assert s - {2} == {1, 3}
assert s ^ {2, 4} == {1, 3, 4}

fs = frozenset([1, 2, 3])
assert hash(fs) == hash(frozenset([3, 2, 1]))
assert fs <= frozenset([1, 2, 3, 4])
assert frozenset() < fs


# ---------- OrderedDict ----------
from collections import OrderedDict

od = OrderedDict()
od["a"] = 1
od["b"] = 2
od["c"] = 3
assert list(od) == ["a", "b", "c"]

od.move_to_end("a")
assert list(od) == ["b", "c", "a"]

od.move_to_end("c", last=False)
assert list(od) == ["c", "b", "a"]

assert od.popitem() == ("a", 1)
assert od.popitem(last=False) == ("c", 3)
assert list(od) == ["b"]

# Equality: OrderedDict compares order-sensitively with another
# OrderedDict but order-insensitively with a plain dict.
a = OrderedDict([("a", 1), ("b", 2)])
b = OrderedDict([("b", 2), ("a", 1)])
assert a != b
assert a == {"a": 1, "b": 2}
assert a == {"b": 2, "a": 1}


# ---------- defaultdict ----------
from collections import defaultdict

dd = defaultdict(list)
dd["a"].append(1)
dd["a"].append(2)
dd["b"].append(3)
assert dd["a"] == [1, 2]
assert dd["b"] == [3]
assert dd["c"] == []
assert "c" in dd

dd2 = defaultdict(int)
for ch in "mississippi":
    dd2[ch] += 1
assert dd2["s"] == 4
assert dd2["m"] == 1


# ---------- Counter ----------
from collections import Counter

c = Counter("mississippi")
assert c["s"] == 4
assert c["i"] == 4
assert c["p"] == 2
assert c["m"] == 1
assert c["z"] == 0

top = c.most_common(2)
assert top[0][1] == 4
assert top[1][1] == 4

c2 = Counter("missouri")
combined = c + c2
assert combined["s"] == 6, combined["s"]
assert combined["i"] == 6, combined["i"]

diff = c - c2
assert diff["s"] == 2
assert diff["p"] == 2

inter = c & c2
assert inter["s"] == 2
assert inter["i"] == 2

uni = c | c2
assert uni["s"] == 4
assert uni["o"] == 1


# ---------- ChainMap ----------
from collections import ChainMap

base = {"a": 1, "b": 2}
override = {"b": 99}
cm = ChainMap(override, base)
assert cm["a"] == 1
assert cm["b"] == 99
assert list(cm) == ["b", "a"] or list(cm) == ["a", "b"]
assert dict(cm) == {"a": 1, "b": 99}

cm["c"] = 3
assert override == {"b": 99, "c": 3}

child = cm.new_child({"x": 100})
assert child["x"] == 100
assert child["a"] == 1
assert child.parents.maps == cm.maps


# ---------- deque ----------
from collections import deque

dq = deque([1, 2, 3])
dq.append(4)
dq.appendleft(0)
assert list(dq) == [0, 1, 2, 3, 4]
dq.rotate(1)
assert list(dq) == [4, 0, 1, 2, 3]
dq.rotate(-2)
assert list(dq) == [1, 2, 3, 4, 0]

dq2 = deque(maxlen=3)
for v in range(5):
    dq2.append(v)
assert list(dq2) == [2, 3, 4]
assert dq2.maxlen == 3

dq.pop()
dq.popleft()
assert list(dq) == [2, 3, 4]
dq.extend([5, 6])
dq.extendleft([1, 0])
assert list(dq) == [0, 1, 2, 3, 4, 5, 6]


# ---------- heapq ----------
import heapq

data = [5, 3, 8, 1, 9, 2]
heap = list(data)
heapq.heapify(heap)
assert heapq.heappop(heap) == 1
heapq.heappush(heap, 0)
assert heapq.heappop(heap) == 0

assert sorted(data) == sorted(heapq.merge(sorted(data[:3]), sorted(data[3:])))
assert heapq.nsmallest(3, data) == [1, 2, 3]
assert heapq.nlargest(3, data) == [9, 8, 5]
assert heapq.nsmallest(2, data, key=lambda x: -x) == [9, 8]


# ---------- bisect ----------
import bisect

xs = [1, 3, 5, 7, 9]
assert bisect.bisect_left(xs, 5) == 2
assert bisect.bisect_right(xs, 5) == 3
assert bisect.bisect(xs, 6) == 3

ys = []
for v in [3, 1, 4, 1, 5, 9, 2, 6]:
    bisect.insort(ys, v)
assert ys == [1, 1, 2, 3, 4, 5, 6, 9]

# bisect with key=
pairs = [(1, "a"), (3, "c"), (5, "e")]
i = bisect.bisect_left(pairs, 4, key=lambda p: p[0])
assert i == 2


# ---------- array.array ----------
import array

a = array.array("i", [1, 2, 3, 4])
assert a.typecode == "i"
assert len(a) == 4
assert a[0] == 1
assert a[-1] == 4
assert list(a) == [1, 2, 3, 4]

a.append(5)
a.extend([6, 7])
assert list(a) == [1, 2, 3, 4, 5, 6, 7]

a.insert(0, 0)
assert a[0] == 0
assert a.pop() == 7

a2 = array.array("i", [10, 20])
combined = a + a2
assert combined.typecode == "i"
assert list(combined) == list(a) + [10, 20]

doubled = a * 2
assert list(doubled) == list(a) + list(a)

# tobytes / frombytes round-trip
raw = a.tobytes()
b = array.array("i")
b.frombytes(raw)
assert list(b) == list(a)

# different typecodes
fa = array.array("d", [1.5, 2.5, 3.5])
assert fa.itemsize == 8
assert list(fa) == [1.5, 2.5, 3.5]

ba = array.array("b", [-1, 0, 1])
assert ba.itemsize == 1
assert list(ba) == [-1, 0, 1]

# slicing returns array.array, not list
sl = a[1:4]
assert isinstance(sl, array.array)
assert list(sl) == [1, 2, 3]

# index / count
a3 = array.array("i", [1, 2, 3, 2, 1])
assert a3.index(2) == 1
assert a3.count(2) == 2


# ---------- types.MappingProxyType ----------
from types import MappingProxyType

m = MappingProxyType({"a": 1})
assert m["a"] == 1
assert list(m) == ["a"]

try:
    m["b"] = 2
except TypeError:
    pass
else:
    assert False, "MappingProxyType must reject assignment"


print("test_containers_array: OK")
