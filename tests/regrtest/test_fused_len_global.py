"""`len(local)` in compiled code keeps `len`'s semantics after its site
caches the fused shape: the length of whatever the local holds now, a
class's `__len__`, the `TypeError` of an object without one, and a
global `len` that shadows the builtin."""


class Sized:
    def __init__(self, n):
        self.n = n

    def __len__(self):
        return self.n


def lengths(values):
    # A generator body, as `contextlib.contextmanager` bodies are.
    for v in values:
        yield len(v)


def run(values):
    return list(lengths(values))


data = [1, 2, 3]
mixed = [data, (1,), "héllo", {"a": 1}, Sized(7), [], b"xy", range(4)]
# Past the frame JIT's threshold, so later rounds run compiled code.
for i in range(3000):
    assert run(mixed) == [3, 1, 5, 1, 7, 0, 2, 4], run(mixed)
data.append(4)
assert run([data]) == [4]

try:
    run([1])
except TypeError as e:
    assert "has no len()" in str(e), e
else:
    raise AssertionError("len(1) didn't raise")


# A module global `len` shadows the builtin from then on.
def shadow(n):
    return n * 10


g = globals()
g["len"] = shadow
try:
    assert run([2, 3]) == [20, 30], run([2, 3])
finally:
    del g["len"]
assert run([[1], (1, 2)]) == [1, 2]
print("ok")
