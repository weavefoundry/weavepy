def g(*args, **kwargs):
    return sum(args), sorted(kwargs.items())


print(g(1, 2, 3, x=10, y=20))


def h(a, *args, kw=5):
    return a, args, kw


print(h(1, 2, 3, kw=99))
print(h(1, 2, 3))
