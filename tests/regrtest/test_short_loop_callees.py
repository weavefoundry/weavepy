"""Functions whose loops run a few iterations a call (a filtered
comprehension over a short split, a small `for` that sums) give the same
results however they run: called from compiled loops, from the interpreter,
after their loops grow long, with a global they read rebound, raising from
inside the loop, and recursing."""


def parts(s):
    return [x for x in s.split("/") if x]


def total(xs):
    t = 0
    for x in xs:
        t += x
    return t


SCALE = 1


def scaled(xs):
    out = []
    for x in xs:
        out.append(x * SCALE)
    return out


def first_bad(xs):
    for x in xs:
        if x < 0:
            raise ValueError(x)
    return len(xs)


def depth(xs):
    n = 0
    for x in xs:
        if isinstance(x, list):
            n = max(n, depth(x))
    return n + 1


def run(n):
    acc = 0
    for i in range(n):
        acc += len(parts("/srv/app/%d/file.txt" % i))
        acc += total((i, 1, 2, 3))
        acc += sum(scaled([1, 2, i]))
        try:
            first_bad([1, 2, -i - 1])
        except ValueError as e:
            acc += e.args[0]
        acc += depth([1, [2, [3]], i])
    return acc


def expected(n, scale=1):
    acc = 0
    for i in range(n):
        acc += 4 + (i + 6) + (3 + i) * scale - (i + 1) + 3
    return acc


for _ in range(6):
    assert run(400) == expected(400)

# Long loops now, through the same functions.
assert total(range(10000)) == sum(range(10000))
assert len(parts("/".join("abc" for _ in range(500)))) == 500
assert run(50) == expected(50)

# A global the callee reads, rebound.
SCALE = 3
assert run(50) == expected(50, 3)
SCALE = 1
assert run(50) == expected(50)
print("ok")
