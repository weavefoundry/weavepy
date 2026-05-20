def f(x):
    try:
        if x == 0:
            raise ValueError("zero")
        return "ok"
    finally:
        print("cleanup", x)


print(f(1))
try:
    f(0)
except ValueError:
    print("caught")
