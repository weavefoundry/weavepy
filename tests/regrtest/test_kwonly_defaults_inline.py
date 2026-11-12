"""Calls of functions with keyword-only parameters from compiled loops bind
those parameters to their defaults as the interpreter does: positional
calls, with positional defaults too, methods, keyword calls, rebound or
deleted `__kwdefaults__` and `__defaults__`, a mutable default, and a
missing default's `TypeError`."""

lst = [1, 2, 3]


def kw(a, *, c=3):
    return len(lst) + a + c


def both(a, b=10, *, c=3, d=4):
    return a + b + c + d


def mutable(a, *, acc=[]):
    acc.append(a)
    return len(acc)


class K:
    def m(self, a, *, scale=2):
        return a * scale


def run(n):
    k = K()
    total = 0
    for i in range(n):
        total += kw(i)
        total += both(i)
        total += both(i, 1)
        total += k.m(i)
        total += kw(i, c=0)
    return total


def expected(n):
    total = 0
    for i in range(n):
        total += 3 + i + 3
        total += i + 10 + 3 + 4
        total += i + 1 + 3 + 4
        total += i * 2
        total += 3 + i
    return total


for _ in range(5):
    assert run(300) == expected(300)

# Rebinding the defaults.
kw.__kwdefaults__ = {"c": 100}
assert run(10) == expected(10) + 97 * 10, (run(10), expected(10))
kw.__kwdefaults__ = {"c": 3}
assert run(10) == expected(10)
both.__defaults__ = (20,)
assert run(10) == expected(10) + 10 * 10
both.__defaults__ = (10,)
assert run(10) == expected(10)

# A deleted default raises where the call is.
kw.__kwdefaults__ = None
try:
    run(1)
except TypeError as e:
    assert "c" in str(e), e
else:
    raise AssertionError("expected TypeError")
kw.__kwdefaults__ = {"c": 3}
assert run(10) == expected(10)


# The same default object every call.
def fill(n):
    last = 0
    for i in range(n):
        last = mutable(i)
    return last


for _ in range(4):
    before = len(mutable.__kwdefaults__["acc"])
    assert fill(200) == before + 200

# A method's keyword-only default rebound on the function.
K.m.__kwdefaults__ = {"scale": 3}
k = K()
assert sum(k.m(i) for i in range(10)) == 3 * 45


def method_loop(n):
    k = K()
    return sum([k.m(i) for i in range(n)])


assert method_loop(100) == 3 * 4950
print("ok")
