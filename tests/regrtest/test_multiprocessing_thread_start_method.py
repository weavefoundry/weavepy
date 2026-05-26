"""RFC 0026 — multiprocessing with start_method='thread'.

In environments where fork/exec is too expensive (or impossible —
sandboxed CI, WASM), `set_start_method("thread")` keeps the API
working by running each "process" inside a fresh worker thread of
the current interpreter. The contract: same Process API, no real
process boundary, exitcode reflects the worker's outcome.
"""

import multiprocessing
import threading
import time


_RESULT = {"value": None}


def writer(value):
    _RESULT["value"] = value


def raiser():
    raise RuntimeError("boom")


def main():
    multiprocessing.set_start_method("thread", force=True)
    assert multiprocessing.get_start_method() == "thread"

    # --- happy path: worker thread mutates a shared dict ----------------
    p = multiprocessing.Process(target=writer, args=("hello-thread",))
    p.start()
    p.join(timeout=2.0)
    assert _RESULT["value"] == "hello-thread"
    assert not p.is_alive()
    assert p.exitcode == 0

    # --- failing worker propagates a non-zero exitcode ------------------
    p = multiprocessing.Process(target=raiser)
    p.start()
    p.join(timeout=2.0)
    assert p.exitcode == 1

    # --- multiple workers run concurrently ------------------------------
    counter = {"n": 0}
    lock = threading.Lock()

    def bump():
        for _ in range(100):
            with lock:
                counter["n"] += 1

    workers = [multiprocessing.Process(target=bump) for _ in range(4)]
    for w in workers:
        w.start()
    for w in workers:
        w.join(timeout=5.0)
        assert w.exitcode == 0
    assert counter["n"] == 400

    # Reset for future tests.
    multiprocessing.set_start_method("spawn", force=True)
    print("multiprocessing thread start method ok")


if __name__ == "__main__":
    main()
