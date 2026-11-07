"""`a + b` and `a += b` of strings, tuples and lists between locals in
compiled code (a site that has made a string keeps its result on the
stack), keep CPython's results: the joined values, a site that sees
numbers first and strings later or the reverse, a list's `+=` extending
the list itself, and a type error."""


def join(a, *parts):
    sep = "/"
    path = a
    for b in parts:
        if b.startswith(sep) or not path:
            path = b
        elif path.endswith(sep):
            path += b
        else:
            path += sep + b
    return path


def add(x, y):
    return x + y


def iadd(x, y):
    x += y
    return x


def run(i):
    assert join("/srv", "app", "d%d" % i) == "/srv/app/d%d" % i
    assert join("a/", "b") == "a/b" and join("", "x") == "x" and join("a", "/b") == "/b"
    assert add("p", str(i)) == "p" + str(i) and add(i, 2) == i + 2
    assert add((1,), (i,)) == (1, i) and add([1], [i]) == [1, i]
    assert add(1.5, i) == 1.5 + i
    xs = [0]
    assert iadd(xs, [i]) is xs and xs == [0, i]
    assert iadd("s", "t") == "st" and iadd(i, 1) == i + 1 and iadd((i,), ()) == (i,)
    try:
        add("a", i)
    except TypeError as e:
        assert "can only concatenate str" in str(e), e
    else:
        raise AssertionError("str + int was taken")


# Past the frame JIT's threshold, so later rounds run compiled code.
for i in range(3000):
    run(i)
print("ok")
