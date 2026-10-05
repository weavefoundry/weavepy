"""pickle.dumps/loads of mixed containers and class instances."""

import pickle

WORK = 600


class Record:
    def __init__(self, i):
        self.ident = i
        self.label = "rec-%d" % i
        self.values = [i * 0.5, i * 1.5]
        self.attrs = {"even": i % 2 == 0}


def make():
    return {
        "records": [Record(i) for i in range(50)],
        "ints": list(range(200)),
        "strs": ["s%d" % i for i in range(100)],
        "nested": [(i, str(i), [i]) for i in range(50)],
        "big": 1 << 100,
    }


def bench(n):
    data = make()
    total = 0
    for _ in range(n):
        s = pickle.dumps(data, protocol=pickle.HIGHEST_PROTOCOL)
        back = pickle.loads(s)
        total += len(s) + len(back["records"]) + back["records"][7].ident
    return total
