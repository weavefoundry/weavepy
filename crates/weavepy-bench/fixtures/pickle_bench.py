"""`pickle.dumps`/`pickle.loads` round trips (RFC 0077 WS6).

The accelerator census fixture for `_pickle`: CPython runs these on
the C pickler; WeavePy on the pure-Python `pickle` module. Nested
containers of scalars and strings plus a small class instance graph,
protocol 4 and 5, mirror what `multiprocessing` queues and cache
layers actually serialize.
"""

import os
import pickle


class Point:
    __slots__ = ("x", "y", "tag")

    def __init__(self, x, y, tag):
        self.x = x
        self.y = y
        self.tag = tag


class Record:
    def __init__(self, i):
        self.ident = i
        self.name = "record-%d" % i
        self.points = [Point(i + k, i - k, "p%d" % k) for k in range(4)]
        self.meta = {"kind": "rec", "seq": i, "flags": (True, False, None)}


def bench(n):
    total = 0
    payload = {
        "ints": list(range(200)),
        "floats": [x * 0.5 for x in range(100)],
        "strs": ["item-%d" % i for i in range(100)],
        "nested": {str(i): {"a": i, "b": [i, i + 1, i + 2]} for i in range(50)},
        "tuples": [(i, i * 2, "t") for i in range(50)],
        "bytes": bytes(range(256)) * 4,
    }
    records = [Record(i) for i in range(20)]
    for i in range(n):
        proto = 4 if i & 1 else 5
        blob = pickle.dumps(payload, protocol=proto)
        back = pickle.loads(blob)
        total += len(blob) + len(back["ints"])
        rblob = pickle.dumps(records, protocol=proto)
        rback = pickle.loads(rblob)
        total += rback[3].points[2].x + len(rblob)
    return total


if __name__ == "__main__":
    import time

    n = int(os.environ.get("WEAVEPY_BENCH_WORK", "40"))
    _t0 = time.perf_counter_ns()
    bench(n)
    _t1 = time.perf_counter_ns()
    print("WEAVEPY_BENCH_NS=%d" % (_t1 - _t0))
