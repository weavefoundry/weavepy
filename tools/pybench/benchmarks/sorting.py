"""sorted/list.sort over ints, strings, tuples, keys, and itemgetter;
min/max/sum with keys; and stable multi-key sorts."""

import operator
import random

WORK = 20


def bench(n):
    rng = random.Random(3)
    ints = [rng.randrange(1 << 30) for _ in range(5000)]
    strs = ["k%07d" % rng.randrange(10 ** 7) for _ in range(3000)]
    recs = [{"name": strs[i], "age": i % 90, "score": ints[i] % 1000} for i in range(3000)]
    tups = [(r["age"], r["score"], r["name"]) for r in recs]
    total = 0
    for _ in range(n):
        total += sorted(ints)[2500]
        total += len(sorted(strs, reverse=True)[0])
        total += sorted(tups)[100][1]
        total += sorted(recs, key=operator.itemgetter("score"))[10]["age"]
        total += sorted(recs, key=lambda r: (r["age"], -r["score"]))[50]["score"]
        lst = list(ints)
        lst.sort(key=lambda v: v % 1000)
        total += lst[0] % 1000
        total += max(recs, key=lambda r: r["score"])["score"] + min(ints) % 7
    return total
