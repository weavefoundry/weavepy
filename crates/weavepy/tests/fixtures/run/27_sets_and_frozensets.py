s = {1, 2, 3, 4}
print(sorted(s))
s.add(5)
s.discard(2)
print(sorted(s))

a = {1, 2, 3, 4}
b = {3, 4, 5, 6}
print(sorted(a | b))
print(sorted(a & b))
print(sorted(a - b))
print(sorted(a ^ b))

fs = frozenset({1, 2, 3})
print(sorted(fs))
print(1 in fs, 5 in fs)

evens = {x for x in range(10) if x % 2 == 0}
print(sorted(evens))

empty = set()
empty.add(1)
print(sorted(empty))
