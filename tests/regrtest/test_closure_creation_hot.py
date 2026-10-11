"""Functions and closures made per call in warm code.

A `def` or `lambda` run in a loop is built with its defaults and closure
at once; its cells, defaults, names, module and late binding must match a
function built one attribute at a time.
"""

import gc
import weakref

N = 3000


def make_adder(k):
    def add(x, scale=2):
        return x * scale + k
    return add


def make_counter():
    count = 0

    def bump(step=1):
        nonlocal count
        count += step
        return count
    return bump


def late_binding(n):
    fns = []
    for i in range(n):
        fns.append(lambda: i)
    return [f() for f in fns]


total = 0
for i in range(N):
    add = make_adder(i)
    total += add(1)
    assert add.__defaults__ == (2,)
    assert add.__closure__[0].cell_contents == i
    assert add.__name__ == 'add'
    assert add.__qualname__ == 'make_adder.<locals>.add'
    assert add.__module__ == __name__
    assert add.__kwdefaults__ is None
assert total == sum(2 + i for i in range(N)), total

counter = make_counter()
for i in range(N):
    assert counter() == i + 1
assert counter(10) == N + 10
assert counter.__closure__[0].cell_contents == N + 10

assert late_binding(5) == [4] * 5


def with_kwdefaults(i):
    def f(a, b=i, *, c=i + 1):
        return a, b, c
    return f


for i in range(N):
    f = with_kwdefaults(i)
    assert f(0) == (0, i, i + 1)
    assert f.__kwdefaults__ == {'c': i + 1}


# A closure's function dies with its last reference, and a cycle through
# its cell is collected.
class Sentinel:
    pass


def capture(obj):
    def inner():
        return obj
    return inner


refs = []
for i in range(100):
    s = Sentinel()
    refs.append(weakref.ref(s))
    fn = capture(s)
    assert fn() is s
    del s, fn
assert all(r() is None for r in refs)


def cyclic():
    def me():
        return me
    return me


for i in range(N):
    f = cyclic()
    assert f() is f
w = weakref.ref(f)
del f
gc.collect()
assert w() is None
print('closure creation: ok')
