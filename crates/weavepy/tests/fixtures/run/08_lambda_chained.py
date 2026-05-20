double = lambda x: x * 2
print(double(7))

xs = [3, 1, 4, 1, 5, 9, 2, 6]
xs.sort()
print(xs)

# chained comparisons
for n in [-1, 0, 5, 10, 11]:
    if 0 < n < 10:
        print("in")
    else:
        print("out")

# boolean short circuit
print(True and 1)
print(False or 2)
print(None or "default")
