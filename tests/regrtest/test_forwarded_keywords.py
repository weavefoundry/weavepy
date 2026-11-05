"""`f(*args, **kwargs)` forwarding through a decorator's wrapper, warm
enough to bind a frameless callee's keywords in place, keeps the call's
semantics: values by name, defaults, keyword-only parameters, and the
`TypeError`s of a duplicate, an unknown or a non-string keyword."""
import functools


def counted(fn):
    @functools.wraps(fn)
    def wrapper(*args, **kwargs):
        wrapper.calls += 1
        return fn(*args, **kwargs)

    wrapper.calls = 0
    return wrapper


@counted
def area(w, h=1, *, scale=1.0):
    return w * h * scale


@counted
def collect(*args, **kw):
    return args, kw


# Past the frame JIT's threshold, so later rounds run compiled code.
for i in range(3000):
    assert area(i, h=2, scale=0.5) == i * 1.0
    assert area(3) == 3.0
    assert area(w=2, h=5) == 10.0
    assert area(4, scale=2) == 8
    assert collect(1, x=2) == ((1,), {"x": 2})

    try:
        area(1, w=2)
    except TypeError as e:
        assert "multiple values" in str(e), e
    else:
        raise AssertionError("duplicate keyword didn't raise")

    try:
        area(1, depth=2)
    except TypeError as e:
        assert "unexpected keyword" in str(e), e
    else:
        raise AssertionError("unknown keyword didn't raise")

    try:
        area(1, **{3: 4})
    except TypeError:
        pass
    else:
        raise AssertionError("non-string keyword didn't raise")

assert area.calls == 3000 * 6, area.calls
print("ok")
