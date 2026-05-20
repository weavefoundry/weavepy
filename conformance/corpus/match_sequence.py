def first(xs):
    match xs:
        case []:
            return None
        case [a, *_]:
            return a

print(first([]))
print(first([1, 2, 3]))
