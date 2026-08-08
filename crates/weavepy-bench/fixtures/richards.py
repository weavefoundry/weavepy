"""Tiny Richards-style task scheduler — exercises classes,
attribute access, and method dispatch."""

import os


class Task:
    def __init__(self, ident, prio):
        self.ident = ident
        self.prio = prio
        self.run_count = 0

    def run(self):
        self.run_count += 1
        return self.run_count


def bench(n):
    tasks = [Task(i, 10 - i) for i in range(8)]
    for _ in range(n):
        for t in tasks:
            t.run()
    return sum(t.run_count for t in tasks)


if __name__ == "__main__":
    import time

    n = int(os.environ.get("WEAVEPY_BENCH_WORK", "1"))
    _t0 = time.perf_counter_ns()
    bench(n)
    _t1 = time.perf_counter_ns()
    print("WEAVEPY_BENCH_NS=%d" % (_t1 - _t0))
