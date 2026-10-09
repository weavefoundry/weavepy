"""namedtuple, defaultdict, Counter, OrderedDict, ChainMap, and bisect."""

import bisect
from collections import ChainMap, Counter, OrderedDict, defaultdict, namedtuple

WORK = 20000

Point = namedtuple("Point", "x y label")


def bench(n):
    groups = defaultdict(list)
    counter = Counter()
    lru = OrderedDict()
    sorted_keys = []
    base = {"a": 1, "b": 2}
    total = 0
    for i in range(n):
        p = Point(i % 50, i % 37, "p%d" % (i % 11))
        groups[p.label].append(p)
        counter[p.x] += 1
        lru[p.y] = p
        lru.move_to_end(p.y)
        if len(lru) > 20:
            lru.popitem(last=False)
        if i % 10 == 0:
            bisect.insort(sorted_keys, (i * 7919) % 1000)
        cm = ChainMap({"b": i}, base)
        total += cm["a"] + cm["b"] + p.x + p._replace(x=1).x
    total += sum(len(v) for v in groups.values())
    total += counter.most_common(1)[0][1] + len(lru) + bisect.bisect(sorted_keys, 500)
    return total
