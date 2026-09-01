"""RFC 0076 WS12 -- the thread-scaling fixture pair.

An embarrassingly parallel pure-Python kernel (no shared state, no C
escape hatches): the same total work runs once serially on the main
thread and once split across THREADS worker threads. Under the
default GIL build the two walls should be ~equal (scaling ~1x, the
GIL serializes bytecode); under `-X gil=0` the threaded wall should
divide by the usable cores (scaling >1x). The runner
(`weavepy-bench scaling`) launches this file under both modes and
reports scaling = serial / parallel per mode -- the mode's claim as
a measured number, per the RFC's "measured, not marketing" clause.
"""

import os
import threading
import time

THREADS = 8


def kernel(n):
    # Pure-Python integer arithmetic: no allocations beyond small
    # ints, no shared state -- the workload is the interpreter loop
    # itself. Deliberately +/*/% only: the bitwise operators (^, >>,
    # &) measured 5x slower serially *and* fully serialized across
    # threads under gil=0 (a contended dispatch path, not the GIL) --
    # a real engine finding, but this fixture's job is to measure the
    # scaling the mode delivers on the hot path, so the kernel stays
    # off the known-contended shape (tracked in FREETHREADING.md).
    s = 0
    for i in range(n):
        s = (s + i * i) % 1000000007
    return s


def bench_serial(n):
    t0 = time.perf_counter_ns()
    for _ in range(THREADS):
        kernel(n)
    t1 = time.perf_counter_ns()
    return t1 - t0


def bench_parallel(n):
    # Thread spawn cost stays outside the timed window: every worker
    # parks on the barrier, the timer starts when the main thread's
    # wait() releases them all. Under the default GIL build (JIT-hot
    # kernel, milliseconds of work) spawn otherwise dominates the wall
    # and reports junk sub-1x "scaling".
    barrier = threading.Barrier(THREADS + 1)

    def worker():
        barrier.wait()
        kernel(n)

    threads = [threading.Thread(target=worker) for _ in range(THREADS)]
    for t in threads:
        t.start()
    barrier.wait()
    t0 = time.perf_counter_ns()
    for t in threads:
        t.join()
    t1 = time.perf_counter_ns()
    return t1 - t0


if __name__ == "__main__":
    n = int(os.environ.get("WEAVEPY_BENCH_WORK", "3000000"))
    # Warm the kernel at full size (dropped): the serial leg runs
    # first, and an in-flight tier-2 compile during it would report as
    # fake "scaling" on the parallel leg. Warm the thread machinery
    # too so thread spawn cost isn't the parallel leg's first-run tax.
    kernel(n)
    t = threading.Thread(target=kernel, args=(1,))
    t.start()
    t.join()
    serial = bench_serial(n)
    parallel = bench_parallel(n)
    print(f"WEAVEPY_BENCH_SERIAL_NS={serial}")
    print(f"WEAVEPY_BENCH_PARALLEL_NS={parallel}")
