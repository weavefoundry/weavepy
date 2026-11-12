"""Arithmetic with `bool` operands from compiled code keeps CPython's
results and types: a `bool` counts as its int value, except that `&`,
`|` and `^` of two bools stay a bool."""


def check(i):
    t, u = i % 2 == 0, i % 3 == 0
    ti, ui = int(t), int(u)
    assert t + u == ti + ui and type(t + u) is int
    assert t - 2 == ti - 2 and t * 3.5 == ti * 3.5 and -t == -ti
    assert type(t & u) is bool and (t & u) == (t and u)
    assert type(t | u) is bool and (t | u) == (t or u)
    assert type(t ^ u) is bool and (t ^ u) == (t != u)
    assert 5 & t == 5 & ti and type(5 & t) is int and type(t | 2) is int
    assert i + t == i + ti and t << 3 == ti << 3 and u / 2 == ui / 2
    assert 2**62 + t + 2**62 == 2**63 + ti and (t ** -1 if t else 0) == (1.0 if t else 0)
    acc = 0
    acc += t
    acc += u
    assert acc == ti + ui and type(acc) is int


# Past the frame JIT's threshold, so later rounds run compiled code.
for i in range(3000):
    check(i)
print("ok")
