"""`collections.deque` in its queue and ring shapes (RFC 0077 WS6).

The accelerator census fixture for `_collections`: a FIFO drained from
the left (asyncio's ready queue, `queue.Queue`), a bounded `maxlen`
ring, `rotate`, both-end peeks, and iteration. CPython runs these on
the C deque; WeavePy on its `_collections.py` stand-in.
"""

import os
from collections import deque


def bench(n):
    total = 0
    q = deque()
    for i in range(n):
        q.append(i)
        q.append(i + 1)
        total += q.popleft()
        if i & 7 == 0:
            total += q[0] + q[-1]
    while q:
        total += q.popleft()
    ring = deque(maxlen=64)
    for i in range(n):
        ring.append(i)
        if i & 15 == 15:
            ring.rotate(3)
            total += ring[5]
    total += sum(ring)
    stack = deque()
    for i in range(n):
        stack.appendleft(i)
        if i & 3 == 3:
            total += stack.pop() + stack.popleft()
    total += len(stack)
    for x in stack:
        total += x
    return total


if __name__ == "__main__":
    import time

    n = int(os.environ.get("WEAVEPY_BENCH_WORK", "200000"))
    _t0 = time.perf_counter_ns()
    bench(n)
    _t1 = time.perf_counter_ns()
    print("WEAVEPY_BENCH_NS=%d" % (_t1 - _t0))
