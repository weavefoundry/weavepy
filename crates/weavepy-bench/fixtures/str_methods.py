"""String method churn — split/join/replace/case/format loops."""

import os

BASE = "The quick brown fox jumps over the lazy dog; " * 4


def bench(n):
    total = 0
    for i in range(n):
        s = BASE
        parts = s.split()
        s2 = " ".join(parts)
        s3 = s2.replace("fox", "cat").replace("dog", "hen")
        s4 = s3.upper().lower().title()
        s5 = "%s #%d [%s]" % (s4[:40], i, ",".join(parts[:5]))
        if s5.startswith("The") or s5.endswith("]"):
            total += len(s5)
        total += s3.count("cat") + s2.find("lazy")
    return total


if __name__ == "__main__":
    import time

    n = int(os.environ.get("WEAVEPY_BENCH_WORK", "5000"))
    _t0 = time.perf_counter_ns()
    bench(n)
    _t1 = time.perf_counter_ns()
    print("WEAVEPY_BENCH_NS=%d" % (_t1 - _t0))
