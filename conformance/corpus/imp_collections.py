from collections import Counter, OrderedDict, defaultdict, deque

c = Counter("abracadabra")
print(c.most_common(2))

d = defaultdict(list)
d["x"].append(1)
d["x"].append(2)
print(dict(d))

od = OrderedDict()
od["a"] = 1
od["b"] = 2
print(list(od.items()))

dq = deque([1, 2, 3])
dq.appendleft(0)
dq.append(4)
print(list(dq))
