"""Function attributes and `f(*args, **kwargs)` forwarding in hot code.

Decorators count calls on the wrapper (`wrapper.calls += 1`) and forward
every argument to the wrapped function. Compiled code reads and writes a
function's `__dict__` directly and binds the forwarded mapping without
building the merged copy; these checks keep both exact once the loops are
warm.
"""

import functools

N = 3000


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


total = 0.0
for i in range(N):
    total += area(i, h=2, scale=0.5)
    total += area(3)
assert area.calls == 2 * N, area.calls
assert total == sum(i * 2 * 0.5 + 3 for i in range(N)), total
assert area.__wrapped__.__name__ == 'area' and area.__name__ == 'area'
assert 'calls' in area.__dict__ and '__wrapped__' in area.__dict__


# A replaced `__dict__` is the one compiled reads and writes see.
def bump(f, n):
    for _ in range(n):
        f.hits += 1
    return f.hits


def target():
    pass


target.hits = 0
assert bump(target, N) == N
fresh = {'hits': 100}
target.__dict__ = fresh
assert bump(target, 5) == 105 and fresh['hits'] == 105
assert target.__dict__ is fresh


# A deleted attribute raises AttributeError from the compiled read.
del target.hits
try:
    bump(target, 1)
except AttributeError as e:
    assert 'hits' in str(e), e
else:
    raise AssertionError('missing function attribute read succeeded')


# Dunder names keep the function type's descriptors.
def named():
    pass


def rename(f, n):
    for k in range(n):
        f.__qualname__ = 'q%d' % k
        f.__doc__ = 'd%d' % k
    return f.__qualname__, f.__doc__, f.__dict__


assert rename(named, N) == ('q%d' % (N - 1), 'd%d' % (N - 1), {})


# Forwarded keywords bind by name, and a callee's own `**kw` is a copy.
def collect(a, b=2, **kw):
    kw['seen'] = True
    return a, b, kw


def forward(*args, **kwargs):
    return collect(*args, **kwargs)


for i in range(N):
    kwargs = {'b': i, 'extra': i + 1}
    a, b, kw = forward(i, **kwargs)
    assert (a, b, kw) == (i, i, {'extra': i + 1, 'seen': True})
    assert kwargs == {'b': i, 'extra': i + 1}


# Errors from the forwarded binding are the callee's.
def strict(x, *, flag=False):
    return x, flag


def forward_strict(*args, **kwargs):
    return strict(*args, **kwargs)


for i in range(N):
    assert forward_strict(i, flag=True) == (i, True)
for bad, message in (
    (lambda: forward_strict(1, x=2), "got multiple values for argument 'x'"),
    (lambda: forward_strict(1, nope=2), "got an unexpected keyword argument 'nope'"),
    (lambda: forward_strict(), "missing 1 required positional argument: 'x'"),
    (lambda: forward_strict(1, 2), 'takes 1 positional argument but 2 were given'),
):
    try:
        bad()
    except TypeError as e:
        assert message in str(e), (message, str(e))
    else:
        raise AssertionError(message)


# A forwarded call into a builtin and into a class.
def call_through(f, *args, **kwargs):
    return f(*args, **kwargs)


for i in range(N):
    assert call_through(max, i, 3, key=lambda v: -v) == min(i, 3)
    assert call_through(dict, a=i) == {'a': i}

print('function attributes and forwarding: ok')
