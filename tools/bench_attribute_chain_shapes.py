"""Compare guarded attribute reads across chain depths and storage layouts.

WEAVEPY_CHAIN_DEPTH selects 2, 3, 4, 8, 9, 16, 17, 32, or 33 fields, including the final
value. WEAVEPY_CHAIN_LAYOUT selects dict, slots, or mixed storage. Source
generation happens before timing so every iteration executes explicit reads.
The deeper cases exercise the guarded and cached native fusion boundaries.
"""

import os
import time


class Node:
    def __init__(self, next=None, value=3):
        self.next = next
        self.value = value


class SlotNode:
    __slots__ = ("next", "value")

    def __init__(self, next=None, value=3):
        self.next = next
        self.value = value


depth = int(os.environ.get("WEAVEPY_CHAIN_DEPTH", "8"))
layout = os.environ.get("WEAVEPY_CHAIN_LAYOUT", "dict")
if depth not in (2, 3, 4, 8, 9, 16, 17, 32, 33):
    raise ValueError("unsupported chain depth")
if layout not in ("dict", "slots", "mixed"):
    raise ValueError("unsupported storage layout")

root_expr = ""
for level in reversed(range(depth)):
    kind = "SlotNode" if layout == "slots" or (layout == "mixed" and level % 2) else "Node"
    root_expr = "%s(%s)" % (kind, root_expr)
value_expr = "root" + ".next" * (depth - 1) + ".value"
source = """def bench(n):
    root = %s
    total = 0
    for _ in range(n):
        total += %s
    assert total == 3 * n
    return total
""" % (root_expr, value_expr)
exec(compile(source, "<attribute-chain-shape>", "exec"), globals())


if __name__ == "__main__":
    n = int(os.environ.get("WEAVEPY_BENCH_WORK", "1000000"))
    start = time.perf_counter_ns()
    bench(n)
    print("WEAVEPY_BENCH_NS=%d" % (time.perf_counter_ns() - start))
