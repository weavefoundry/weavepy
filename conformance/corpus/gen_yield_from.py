def inner():
    yield 1
    yield 2

def outer():
    yield from inner()
    yield 3

print(list(outer()))
