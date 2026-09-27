"""Class populations with unused or exercised attribute caches."""

import os
import time

WARM = os.environ.get("WEAVEPY_CLASS_CACHE_WARM") == "1"


class Base:
    def read(self):
        return self.value


def bench(n):
    classes = [type("Item", (Base,), {"value": i}) for i in range(n)]
    assert len(classes) == n
    if WARM:
        instances = [cls() for cls in classes]
        for _ in range(2):
            total = 0
            for instance in instances:
                total += instance.read()
            assert total == n * (n - 1) // 2
    return classes


if __name__ == "__main__":
    n = int(os.environ.get("WEAVEPY_BENCH_WORK", "10000"))
    start = time.perf_counter_ns()
    population = bench(n)
    print("WEAVEPY_BENCH_NS=%d" % (time.perf_counter_ns() - start))
