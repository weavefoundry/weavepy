def f(x):
    match x:
        case 0:
            return "zero"
        case 1:
            return "one"
        case _:
            return "other"

print(f(0))
print(f(1))
print(f(5))
