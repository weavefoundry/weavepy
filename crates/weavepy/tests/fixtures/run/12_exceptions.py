def divide(a, b):
    if b == 0:
        raise ValueError("cannot divide by zero")
    return a // b


try:
    print(divide(10, 2))
    print(divide(7, 0))
except ValueError as e:
    print("caught:", e.args[0])


try:
    {}["missing"]
except KeyError as e:
    print("key:", e.args[0])


try:
    [1, 2][99]
except IndexError as e:
    print("idx:", e.args[0])


try:
    raise RuntimeError("boom")
except Exception:
    print("any Exception")


# `finally` always runs, even on success.
counter = 0
try:
    counter = counter + 1
finally:
    counter = counter + 10
print(counter)
