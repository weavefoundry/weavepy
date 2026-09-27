"""Read two module fields, including allocation, validation, and cleanup.

WEAVEPY_MODULE_ROOT selects a Python-created module or the imported sys module:
python (default) or native. Each call owns a fresh child, so warm measurements
also exercise replacement rather than retaining one benchmark graph forever.
"""

import os
import sys
import time
from types import ModuleType


def python_modules(n):
    root = ModuleType("root")
    root.child = ModuleType("child")
    root.child.value = 3
    total = 0
    for _ in range(n):
        total += root.child.value
    assert total == 3 * n
    return total


def native_module(n):
    root = sys
    missing = object()
    before = getattr(root, "_weavepy_chain_child", missing)
    child = ModuleType("child")
    child.value = 3
    root._weavepy_chain_child = child
    try:
        total = 0
        for _ in range(n):
            total += root._weavepy_chain_child.value
        assert total == 3 * n
        return total
    finally:
        if before is missing:
            del root._weavepy_chain_child
        else:
            root._weavepy_chain_child = before


kind = os.environ.get("WEAVEPY_MODULE_ROOT", "python")
if kind not in ("python", "native"):
    raise ValueError("unsupported module root")
bench = python_modules if kind == "python" else native_module


if __name__ == "__main__":
    n = int(os.environ.get("WEAVEPY_BENCH_WORK", "1000000"))
    start = time.perf_counter_ns()
    bench(n)
    print("WEAVEPY_BENCH_NS=%d" % (time.perf_counter_ns() - start))
