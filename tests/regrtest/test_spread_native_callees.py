"""`f(*args)` and `f(*args, **kwargs)` of native callees from compiled
loops: a saved bound native method (`log.append`, `log.clear`, `d.get`),
a builtin, a class, through a forwarding wrapper and through a closure's
cells, with empty and non-empty keywords, give what a plain call gives,
errors included."""


def wrapper(f):
    def w(*args, **kwargs):
        return f(*args, **kwargs)

    return w


def closure(callback, /, *args, **kwds):
    def w():
        return callback(*args, **kwds)

    return w


class Point:
    def __init__(self, x, y=0):
        self.x = x
        self.y = y


def run(n):
    log = []
    d = {"a": 1}
    append = wrapper(log.append)
    get = wrapper(d.get)
    length = wrapper(len)
    make = wrapper(Point)
    clear = closure(log.clear)
    push = closure(log.append, 7)
    total = 0
    for i in range(n):
        append(i)
        total += get("a") + get("b", 2) + length(log)
        p = make(i, y=1)
        total += p.x + p.y
        push()
        if i % 10 == 9:
            assert clear() is None
            assert log == []
        args = (i,)
        log.append(*args)
        kw = {}
        log.append(*args, **kw)
        total += len(log)
    return total, log


def expected(n):
    log = []
    total = 0
    for i in range(n):
        log.append(i)
        total += 1 + 2 + len(log)
        total += i + 1
        log.append(7)
        if i % 10 == 9:
            log.clear()
        log.append(i)
        log.append(i)
        total += len(log)
    return total, log


for _ in range(4):
    assert run(500) == expected(500)


# Errors raise where the call is.
def errors(n):
    log = []
    append = wrapper(log.append)
    seen = 0
    for i in range(n):
        try:
            append(i, i)
        except TypeError as e:
            assert "append" in str(e), e
            seen += 1
        try:
            append(item=i)
        except TypeError:
            seen += 1
        try:
            log.append(*(1, 2))
        except TypeError:
            seen += 1
    assert log == []
    return seen


for _ in range(4):
    assert errors(300) == 900


# A method of a subclass of a native type, and one the instance shadows.
class MyList(list):
    def append(self, x):
        super().append(x * 2)


def subclass(n):
    out = MyList()
    app = wrapper(out.append)
    for i in range(n):
        app(i)
    return out


for _ in range(4):
    assert subclass(100) == [i * 2 for i in range(100)]

print("ok")
