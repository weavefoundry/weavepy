def f(a, b, c):
    return a + b + c


xs = [1, 2, 3]
print(f(*xs))

kw = {"a": 1, "b": 2, "c": 3}
print(f(**kw))

print(f(*[1, 2], c=3))
