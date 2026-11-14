"""`enumerate` and `zip` over generators from compiled loops give what the
general steps give: items in order, sources consumed exactly as far as
CPython consumes them when one runs out first, a generator that raises or
is closed mid-loop, sources shared between adapters, and the same
generator on both sides of a `zip`."""


def count(n, log=None):
    i = 0
    try:
        while i < n:
            yield i
            i += 1
    finally:
        if log is not None:
            log.append(("done", n, i))


def squares(n):
    for i in range(n):
        yield i * i


def failing(n):
    for i in range(n):
        yield i
    raise ValueError(n)


def enum_sum(n):
    acc = 0
    for i, x in enumerate(count(n)):
        acc += i * x
    return acc


def enum_started(n):
    acc = 0
    for i, x in enumerate(squares(n), 5):
        acc += i + x
    return acc


def enum_tuples(n):
    last = None
    for pair in enumerate(count(n)):
        last = pair
    return last


def zip_two(n):
    acc = 0
    for a, b in zip(count(n), squares(n)):
        acc += a + b
    return acc


def zip_mixed(n):
    xs = list(range(n))
    acc = 0
    for a, b in zip(count(n), xs):
        acc += a * b
    for a, b in zip(xs, squares(n)):
        acc += a + b
    return acc


def zip_tuples(n):
    out = []
    for pair in zip(count(n), squares(n)):
        out.append(pair)
    return out[-1], len(out)


def uneven(n):
    # The longer first source gives one item more than the pairs made;
    # a shorter first source stops the second from being asked.
    long, short = count(n + 5), count(n)
    pairs = 0
    for a, b in zip(long, short):
        pairs += 1
    left = next(long)
    long2, short2 = count(n + 5), count(n)
    for a, b in zip(short2, long2):
        pairs += 1
    return pairs, left, next(long2)


def same_generator(n):
    g = count(2 * n)
    out = 0
    for a, b in zip(g, g):
        assert b == a + 1
        out += 1
    return out


def raising(n):
    seen = 0
    try:
        for i, x in enumerate(failing(n)):
            seen += 1
    except ValueError as e:
        assert e.args == (n,)
    try:
        for a, b in zip(count(n + 3), failing(n)):
            seen += 1
    except ValueError as e:
        assert e.args == (n,)
    return seen


def early_exit(n):
    log = []
    g = count(n, log)
    for i, x in enumerate(g):
        if i == n // 2:
            break
    rest = sum(1 for _ in g)
    return rest, log


def closing(n):
    log = []
    for i, x in enumerate(count(n, log)):
        if i == 3:
            break
    return log


def shared_source(n):
    g = count(n)
    firsts = 0
    for i, x in enumerate(g):
        if i == 2:
            break
    for a, b in zip(g, squares(n)):
        firsts += a
    return firsts


import itertools


def chained(n):
    acc = 0
    for x in itertools.chain(count(n), squares(3), count(n)):
        acc += x
    return acc


def chained_mixed(n):
    out = []
    for x in itertools.chain([7], count(2), (), squares(3), "ab"):
        out.append(x)
    return out


def sliced(n):
    g = count(10 * n)
    acc = 0
    for x in itertools.islice(g, n):
        acc += x
    for x in itertools.islice(g, 3, n, 2):
        acc += x
    return acc, next(g)


def sliced_short(n):
    # The source ends inside the gap and inside the run.
    a = [x for x in itertools.islice(count(5), 2, 100, 3)]
    b = []
    for x in itertools.islice(count(4), 10, 20):
        b.append(x)
    g = count(8)
    c = []
    for x in itertools.islice(g, 3, 3):
        c.append(x)
    return a, b, c, next(g)


def sliced_failing(n):
    seen = []
    try:
        for x in itertools.islice(failing(3), 10):
            seen.append(x)
    except ValueError as e:
        seen.append(e.args)
    try:
        for x in itertools.chain(count(2), failing(1), count(2)):
            seen.append(x)
    except ValueError as e:
        seen.append(e.args)
    return seen


N = 2000
for _ in range(3):
    assert chained(N) == 2 * sum(range(N)) + 5
    assert chained_mixed(N) == [7, 0, 1, 0, 1, 4, "a", "b"]
    # (The second slice stops asking once its next index would pass its
    # stop, so the source stands at the last item it gave, plus one.)
    assert sliced(N) == (4996999, 4000), sliced(N)
    assert sliced_short(N) == ([2], [], [], 3)
    assert sliced_failing(N) == [0, 1, 2, (3,), 0, 1, 0, (1,)]
    assert enum_sum(N) == sum(i * i for i in range(N))
    assert enum_started(N) == sum(i + 5 + i * i for i in range(N))
    assert enum_tuples(N) == (N - 1, N - 1)
    assert enum_tuples(0) is None
    assert zip_two(N) == sum(i + i * i for i in range(N))
    assert zip_mixed(N) == 2 * sum(i * i for i in range(N)) + sum(range(N))
    assert zip_tuples(N) == ((N - 1, (N - 1) ** 2), N)
    assert uneven(N) == (2 * N, N + 1, N)
    assert same_generator(N) == N
    assert raising(N) == 2 * N
    rest, log = early_exit(N)
    assert rest == N - N // 2 - 1 and log == [("done", N, N)], (rest, log)
    assert shared_source(N) == sum(range(3, N))

# A generator dropped mid-loop runs its `finally`.
import gc

for _ in range(50):
    log = closing(10)
gc.collect()
assert log == [("done", 10, 3)], log
