import itertools

print(list(itertools.chain([1, 2], [3, 4])))
print(list(itertools.islice(itertools.count(10), 4)))
print(list(itertools.permutations([1, 2, 3], 2)))
print(list(itertools.combinations([1, 2, 3], 2)))
