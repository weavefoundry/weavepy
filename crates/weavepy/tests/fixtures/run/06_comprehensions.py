squares = [x * x for x in range(6)]
print(squares)

evens = [x for x in range(10) if x % 2 == 0]
print(evens)

nested = [(x, y) for x in range(3) for y in range(3) if x < y]
print(nested)

cubes = {x: x * x * x for x in range(5)}
print(sorted(cubes.items()))
