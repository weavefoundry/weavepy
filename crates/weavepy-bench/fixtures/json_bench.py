"""Stdlib json round-trips over a nested document."""

import json
import os

DOC = {
    "users": [
        {
            "id": i,
            "name": "user-%d" % i,
            "active": i % 3 != 0,
            "score": i * 0.5,
            "tags": ["alpha", "beta", "gamma"][: i % 4],
            "profile": {"city": "city-%d" % (i % 17), "zip": str(10000 + i)},
        }
        for i in range(200)
    ],
    "meta": {"version": 3, "generated": "bench", "count": 200},
}


def bench(n):
    total = 0
    for _ in range(n):
        blob = json.dumps(DOC)
        back = json.loads(blob)
        total += len(back["users"])
    return total


if __name__ == "__main__":
    import time

    n = int(os.environ.get("WEAVEPY_BENCH_WORK", "50"))
    _t0 = time.perf_counter_ns()
    bench(n)
    _t1 = time.perf_counter_ns()
    print("WEAVEPY_BENCH_NS=%d" % (_t1 - _t0))
