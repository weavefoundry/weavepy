"""Native methods bound to builtin receivers read out of containers and
called from long loops (`groups[k].append(x)`, `parts[i].strip()`) give
what the full call gives: appends, pops of an empty list, `str` and
`dict` methods, a method replaced on a subclass instance, an argument
error, and an element whose finalizer runs when `list.remove` drops it."""

from collections import defaultdict


class Mortal:
    log = []

    def __init__(self, v):
        self.v = v

    def __eq__(self, other):
        return isinstance(other, Mortal) and other.v == self.v

    __hash__ = object.__hash__

    def __del__(self):
        Mortal.log.append(self.v)


class MyList(list):
    def append(self, x):
        list.append(self, -x)


def groups(n):
    g = defaultdict(list)
    for i in range(n):
        g[i % 7].append(i)
        if len(g[i % 7]) > 40:
            g[i % 7].pop()
    return sum(len(v) for v in g.values()), g[3][:3]


for _ in range(3):
    assert groups(20000) == (280, [3, 10, 17])


def strs(n):
    parts = [" a ", "b ", " c"]
    d = [{"k": 1}, {}]
    t = 0
    for i in range(n):
        t += len(parts[i % 3].strip())
        t += d[i % 2].get("k", 5)
        t += parts[i % 3].startswith(" ")
    return t


for _ in range(3):
    assert strs(30000) == 30000 + 15000 * (1 + 5) + 20000


def mixed(n):
    lists = [[], MyList()]
    for i in range(n):
        lists[i % 2].append(i)
    return lists[0][:2], lists[1][:2]


for _ in range(3):
    assert mixed(20000) == ([0, 2], [-1, -3])


def errors(n):
    ls = [[]]
    seen = 0
    for i in range(n):
        try:
            ls[0].pop()
        except IndexError:
            seen += 1
        try:
            ls[0].append()
        except TypeError:
            seen += 1
    return seen


for _ in range(3):
    assert errors(20000) == 40000


def removals(n):
    out = 0
    holder = [[]]
    for i in range(n):
        holder[0].append(Mortal(i))
        Mortal.log.clear()
        holder[0].remove(Mortal(i))
        out += Mortal.log.count(i) == 2
    return out


for _ in range(3):
    assert removals(3000) == 3000
print("ok")
