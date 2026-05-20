def counter(n):
    i = 0
    while i < n:
        yield i
        i = i + 1

for x in counter(5):
    print(x)

def flatten(xs):
    for x in xs:
        if isinstance(x, list):
            yield from flatten(x)
        else:
            yield x

nested = [1, [2, [3, 4], 5], 6]
print(list(flatten(nested)))

squares = (n * n for n in range(6))
print(list(squares))

def stopped():
    yield 1
    return "finished"

g = stopped()
print(next(g))
try:
    next(g)
except StopIteration as e:
    print("done:", e.value)
