"""A compiled call of a native method bound to its receiver (`random.random`
is a module's saved `Random().random`): the result matches the method
called through its instance, a raise from it is catchable and leaves the
loop's state intact, and a site that sees other callables keeps working.
A `Random`'s state isn't one of its attributes."""

import random


def module_random(n):
    out = []
    for _ in range(n):
        out.append(random.random())
    return out


def saved_bits(bits, n, k):
    out = []
    for _ in range(n):
        out.append(bits(k))
    return out


def raising(bits, n):
    caught = 0
    for i in range(n):
        try:
            bits(-1)
        except ValueError:
            caught += 1
    return caught


def mixed(fns, n):
    total = 0
    for i in range(n):
        f = fns[i % len(fns)]
        total += f(3)
    return total


random.seed(12345)
a = module_random(2000)
random.seed(12345)
b = [random._inst.random() for _ in range(2000)]
assert a == b, "module random() diverged"
assert all(0.0 <= x < 1.0 for x in a)

r = random.Random(7)
s = random.Random(7)
assert saved_bits(r.getrandbits, 1000, 16) == [s.getrandbits(16) for _ in range(1000)]
assert raising(r.getrandbits, 500) == 500

def triple(x):
    return 3 * x

r2 = random.Random(9)
s2 = random.Random(9)
fns = [r2.getrandbits, triple, abs]
got = mixed(fns, 900)
expect = 0
for i in range(900):
    k = i % 3
    if k == 0:
        expect += s2.getrandbits(3)
    elif k == 1:
        expect += 9
    else:
        expect += 3
assert got == expect, (got, expect)

class Sub(random.Random):
    def random(self):
        return 0.5

def call_saved(f, n):
    return [f() for _ in range(n)]


sub = Sub(1)
assert call_saved(sub.random, 500) == [0.5] * 500
native = random.Random(3)
check = random.Random(3)
assert call_saved(native.random, 500) == [check.random() for _ in range(500)]

# The generator's state isn't an attribute: the instance's `__dict__`
# holds only what Python stored there, and a copy draws the same numbers.
import copy

r3 = random.Random(11)
assert vars(r3) == {"gauss_next": None}, sorted(vars(r3))
r4 = copy.deepcopy(r3)
assert [r3.random() for _ in range(50)] == [r4.random() for _ in range(50)]
state = r3.getstate()
x = [r3.random() for _ in range(10)]
r3.setstate(state)
assert [r3.random() for _ in range(10)] == x

print("ok")
