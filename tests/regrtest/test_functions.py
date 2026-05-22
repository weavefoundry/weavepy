"""Smoke test: function definitions, closures, decorators, *args/**kwargs."""

def add(a, b):
    return a + b

assert add(1, 2) == 3
assert add(b=2, a=1) == 3

def greet(name="world", *, greeting="hello"):
    return f"{greeting}, {name}"

assert greet() == "hello, world"
assert greet("alice") == "hello, alice"
assert greet(greeting="hi") == "hi, world"

def collect(*args, **kwargs):
    return args, sorted(kwargs.items())

a, k = collect(1, 2, 3, x=1, y=2)
assert a == (1, 2, 3)
assert k == [("x", 1), ("y", 2)]

# closures + nested scopes
def make_counter(start=0):
    n = start
    def step(by=1):
        nonlocal n
        n += by
        return n
    return step

c = make_counter()
assert c() == 1
assert c() == 2
assert c(10) == 12

c2 = make_counter(100)
assert c2() == 101

# decorators
def upper(fn):
    def wrapper(*a, **k):
        return fn(*a, **k).upper()
    return wrapper

@upper
def shout(msg):
    return msg

assert shout("hi") == "HI"

# parameterised decorator
def repeat(times):
    def deco(fn):
        def wrapper(*a, **k):
            return [fn(*a, **k) for _ in range(times)]
        return wrapper
    return deco

@repeat(3)
def hi():
    return "yo"

assert hi() == ["yo", "yo", "yo"]

# default arg evaluated once
def buggy(x, acc=[]):
    acc.append(x)
    return acc

assert buggy(1) == [1]
assert buggy(2) == [1, 2]
assert buggy(3, []) == [3]

# generators
def squares(n):
    for i in range(n):
        yield i * i

assert list(squares(5)) == [0, 1, 4, 9, 16]

# generator expression
assert sum(x * x for x in range(10)) == 285

# lambda
assert (lambda a, b: a + b)(2, 3) == 5
assert sorted([(1, 'b'), (2, 'a')], key=lambda t: t[1]) == [(2, 'a'), (1, 'b')]
