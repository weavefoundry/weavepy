"""Consumers that drain a generator (sum, list, tuple, join) see exactly its
own yields, even when it resumes other generators."""


def inner():
    yield 1
    yield 2


def outer():
    for x in inner():
        pass
    yield 10


def outer_sum():
    yield sum(inner())
    yield 5


def outer_list():
    yield list(inner())
    yield [3]


# A materialized frame sends the outer resume down the general path; the
# inner generator's resumes must not fold into the outer consumer.
g = outer()
frame = g.gi_frame
frame.f_locals
assert sum(g) == 10
g = outer()
frame = g.gi_frame
frame.f_locals
assert list(g) == [10]
del frame
assert sum(outer()) == 10
assert list(outer()) == [10]
assert sum(outer_sum()) == 8
assert list(outer_list()) == [[1, 2], [3]]

assert list(x * 2 for x in range(5)) == [0, 2, 4, 6, 8]
assert tuple(str(x) for x in range(3)) == ("0", "1", "2")
assert "".join(chr(65 + x) for x in range(3)) == "ABC"
assert sorted(-x for x in range(4)) == [-3, -2, -1, 0]
assert list(x for x in ()) == []


class Tracked:
    alive = 0

    def __init__(self):
        Tracked.alive += 1

    def __del__(self):
        Tracked.alive -= 1


items = list(Tracked() for _ in range(100))
assert Tracked.alive == 100
del items
assert Tracked.alive == 0


def raises_midway():
    yield 1
    yield 2
    raise KeyError("stop")


try:
    list(raises_midway())
except KeyError as e:
    assert e.args == ("stop",)
else:
    raise AssertionError("list() swallowed the error")


def returns_value():
    yield 1
    return "done"


assert list(returns_value()) == [1]
print("ok")
