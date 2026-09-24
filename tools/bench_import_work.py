"""Measure imports together with first use or sustained module-body work.

WEAVEPY_IMPORT_WORK_KIND selects first_use, module_loop, or module_calls.
All imports, temporary source creation, execution, and cleanup are timed.
"""
import os
import time

KIND = os.environ.get("WEAVEPY_IMPORT_WORK_KIND", "first_use")
EXPECTED_PATH = "a\\b" if os.name == "nt" else "a/b"


def bench(n):
    if KIND == "first_use":
        import collections
        import datetime
        import json
        import pathlib

        total = 0
        for _ in range(n):
            payload = json.loads(json.dumps({"items": list(range(100))}))
            assert payload["items"][-1] == 99
            assert datetime.date(2026, 9, 24).isoformat() == "2026-09-24"
            assert str(pathlib.Path("a") / "b") == EXPECTED_PATH
            assert collections.Counter("ababa")["a"] == 3
            total += payload["items"][-1]
        assert total == 99 * n
        return total
    if KIND not in ("module_loop", "module_calls"):
        raise ValueError("unknown import work kind: " + KIND)
    import sys
    import tempfile

    if KIND == "module_loop":
        source = "def work(n):\n    s = 0\n    for i in range(n):\n        s += i\n    return s\nvalue = work(%d)\n" % n
        expected = n * (n - 1) // 2
    else:
        source = "def leaf(x):\n    return x + 1\ndef work(n):\n    s = 0\n    for i in range(n):\n        s += leaf(i)\n    return s\nvalue = work(%d)\n" % n
        expected = n * (n + 1) // 2
    with tempfile.TemporaryDirectory() as directory:
        with open(os.path.join(directory, "_bench_import_work.py"), "w") as stream:
            stream.write(source)
        sys.path.insert(0, directory)
        try:
            module = __import__("_bench_import_work")
            assert module.value == expected, (module.value, expected)
            return module.value
        finally:
            sys.path.remove(directory)
            sys.modules.pop("_bench_import_work", None)


if __name__ == "__main__":
    n = int(os.environ.get("WEAVEPY_BENCH_WORK", "1"))
    start = time.perf_counter_ns()
    bench(n)
    print("WEAVEPY_BENCH_NS=%d" % (time.perf_counter_ns() - start))
