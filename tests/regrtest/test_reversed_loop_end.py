"""The end of a `for x in reversed(xs)` loop in compiled code.

An exhausted reversed iterator that only the loop holds leaves the stack
in place; one shared with the loop's body, a list subclass's, and a list
that shrank mid-loop end as in CPython. Run it past the compile
threshold.
"""


class L(list):
    pass


def total(n, xs):
    t = 0
    for _ in range(n):
        for x in reversed(xs):
            t += x
    return t


def shared(xs):
    it = reversed(xs)
    out = []
    for x in it:
        out.append(x)
        out.append(next(it, None))
    return out, list(it)


def shrink(xs):
    out = []
    for x in reversed(xs):
        out.append(x)
        if xs:
            xs.pop(0)
    return out


N = 3000
assert total(N, list(range(10))) == 45 * N
assert total(N, L(range(10))) == 45 * N
assert total(N, []) == 0
for _ in range(N // 10):
    assert shared([1, 2, 3, 4, 5]) == ([5, 4, 3, 2, 1, None], [])
    assert shrink([1, 2, 3, 4]) == [4, 4, 4, 4]
print("ok")
