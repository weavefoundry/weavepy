"""Smoke test: list / dict / set / tuple machinery."""

xs = [1, 2, 3]
xs.append(4)
assert xs == [1, 2, 3, 4]
xs.extend([5, 6])
assert xs == [1, 2, 3, 4, 5, 6]
xs.insert(0, 0)
assert xs == [0, 1, 2, 3, 4, 5, 6]
assert xs.pop() == 6
assert xs.pop(0) == 0
xs.reverse()
assert xs == [5, 4, 3, 2, 1]
xs.sort()
assert xs == [1, 2, 3, 4, 5]
assert xs[1:3] == [2, 3]
assert [x * 2 for x in xs] == [2, 4, 6, 8, 10]
assert [x for x in xs if x % 2] == [1, 3, 5]

d = {"a": 1, "b": 2}
d["c"] = 3
assert d == {"a": 1, "b": 2, "c": 3}
assert set(d.keys()) == {"a", "b", "c"}
assert set(d.values()) == {1, 2, 3}
assert sorted(d.items()) == [("a", 1), ("b", 2), ("c", 3)]
del d["a"]
assert "a" not in d
assert d.get("missing", -1) == -1
d2 = {"x": 10, **d}
assert d2 == {"x": 10, "b": 2, "c": 3}

s = {1, 2, 3}
s.add(4)
assert s == {1, 2, 3, 4}
assert s & {2, 3, 5} == {2, 3}
assert s | {5} == {1, 2, 3, 4, 5}
assert s - {2} == {1, 3, 4}
assert {x for x in range(5) if x % 2 == 0} == {0, 2, 4}

t = (1, 2, 3)
assert t + (4,) == (1, 2, 3, 4)
a, b, c = t
assert (a, b, c) == (1, 2, 3)
a, *rest = [1, 2, 3, 4, 5]
assert a == 1 and rest == [2, 3, 4, 5]
first, *mid, last = (1, 2, 3, 4, 5)
assert first == 1 and mid == [2, 3, 4] and last == 5
