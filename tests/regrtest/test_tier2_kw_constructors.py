"""A class called with keyword arguments from a loop tier 2 compiled
constructs what the interpreter's call constructs: defaults, keyword-only
parameters, `**kwargs`, `__slots__`, dataclasses, and the errors (a missing
or unexpected keyword, an `__init__` that returns a value or raises), with
classes whose construction isn't the default pipeline (`__new__`, a
metaclass, `object.__init__`, an exception) left to the general call."""

import dataclasses


class P:
    def __init__(self, x, y, z):
        self.x = x
        self.y = y
        self.z = z


class D:
    def __init__(self, x, y=2, *, z=3, **rest):
        self.x = x
        self.y = y
        self.z = z
        self.rest = rest


class S:
    __slots__ = ("a", "b")

    def __init__(self, a, b=0):
        self.a = a
        self.b = b


@dataclasses.dataclass
class DC:
    x: int
    y: int = 5
    tags: list = dataclasses.field(default_factory=list)


class Counted:
    live = 0

    def __init__(self, v):
        Counted.live += 1
        self.v = v

    def __del__(self):
        Counted.live -= 1


class New:
    made = 0

    def __new__(cls, v=0):
        New.made += 1
        return super().__new__(cls)

    def __init__(self, v=0):
        self.v = v


class Meta(type):
    def __call__(cls, **kw):
        return ("meta", sorted(kw))


class WithMeta(metaclass=Meta):
    def __init__(self, a):
        self.a = a


class Bare:
    pass


class Err(Exception):
    def __init__(self, msg, code=0):
        super().__init__(msg)
        self.code = code


class BadInit:
    def __init__(self, v):
        self.v = v
        return v  # noqa: PLE0101


class Raises:
    def __init__(self, v):
        if v % 1000 == 999:
            raise ValueError(v)
        self.v = v


def plain(n):
    acc = 0
    for i in range(n):
        p = P(i, y=2, z=3)
        q = P(z=i, y=1, x=0)
        acc += p.x + p.y + p.z + q.z + q.y + q.x
    return acc


def defaults(n):
    acc = 0
    for i in range(n):
        a = D(i, z=7)
        b = D(x=i, y=1, extra=i)
        c = D(i)
        acc += a.x + a.y + a.z + b.y + b.z + b.rest["extra"] + c.z + len(a.rest)
    return acc


def slots(n):
    acc = 0
    for i in range(n):
        s = S(a=i, b=1)
        t = S(a=i)
        acc += s.a + s.b + t.a + t.b
    return acc


def dataclass(n):
    acc = 0
    last = None
    for i in range(n):
        d = DC(x=i, y=2)
        e = DC(x=i)
        e.tags.append(i)
        acc += d.x + d.y + e.y + len(e.tags) + len(d.tags)
        last = e
    return acc, last


def counted(n):
    for i in range(n):
        c = Counted(v=i)
        assert c.v == i
    del c
    return Counted.live


def other_pipelines(n):
    out = None
    for i in range(n):
        a = New(v=i)
        b = WithMeta(a=i, b=2)
        e = Err("m", code=i)
        out = (a.v, b, e.code, e.args)
    return out


def missing(n):
    hits = 0
    for i in range(n):
        try:
            P(i, y=2)
        except TypeError as e:
            assert "z" in str(e), e
            hits += 1
    return hits


def unexpected(n):
    hits = 0
    for i in range(n):
        try:
            P(i, y=2, z=3, w=4)
        except TypeError as e:
            assert "w" in str(e), e
            hits += 1
        try:
            Bare(v=i)
        except TypeError as e:
            assert "takes no arguments" in str(e), e
            hits += 1
    return hits


def bad_init(n):
    hits = 0
    for i in range(n):
        try:
            BadInit(v=i + 1)
        except TypeError as e:
            assert str(e) == "__init__() should return None, not 'int'", e
            hits += 1
    return hits


def raises(n):
    ok = bad = 0
    for i in range(n):
        try:
            r = Raises(v=i)
            ok += r.v == i
        except ValueError as e:
            assert e.args == (i,)
            bad += 1
    return ok, bad


N = 6000
for _ in range(3):
    assert plain(N) == sum(i + 5 + i + 1 for i in range(N))
    assert defaults(N) == sum(i + 2 + 7 + 1 + 3 + i + 3 + 0 for i in range(N))
    assert slots(N) == sum(i + 1 + i for i in range(N))
    acc, last = dataclass(N)
    assert acc == sum(i + 2 + 5 + 1 for i in range(N))
    assert last == DC(x=N - 1, y=5, tags=[N - 1])
    assert counted(N) == 0
    made = New.made
    assert other_pipelines(N) == (N - 1, ("meta", ["a", "b"]), N - 1, ("m",))
    assert New.made == made + N
    assert missing(N) == N
    assert unexpected(N) == 2 * N
    assert bad_init(N) == N
    assert raises(N) == (N - N // 1000, N // 1000)

# The class's `__init__` replaced, and the class given a `__new__`, between
# calls from the same compiled loop.
def rebound(n):
    acc = 0
    for i in range(n):
        if i == n // 2:
            P.__init__ = lambda self, x, y, z: setattr(self, "x", -1)
        acc += P(i, y=0, z=0).x
    return acc


assert rebound(N) == sum(range(N // 2)) - (N - N // 2)
