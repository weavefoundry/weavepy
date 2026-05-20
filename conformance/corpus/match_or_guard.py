def label(n):
    match n:
        case 0 | 1 | 2:
            return "small"
        case n if n > 100:
            return "big"
        case _:
            return "medium"

print(label(1))
print(label(50))
print(label(200))
