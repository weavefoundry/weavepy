"""RFC 0040 — `multiprocessing.dummy` (thread-backed Process API).

CPython's `multiprocessing.dummy` mirrors the full `multiprocessing`
surface on top of `threading`, for environments where a real process
boundary is undesirable (shared state, no fork/exec cost). The contract:
the same `Process`/`Queue`/`Pool` API, no real process boundary, and the
worker's outcome reflected back. This is the faithful CPython equivalent
of "run each worker in a thread" — there is no `set_start_method("thread")`
in CPython.
"""

import multiprocessing.dummy as dummy
import threading


_RESULT = {"value": None}


def writer(value):
    _RESULT["value"] = value


def main():
    # --- happy path: worker thread mutates shared state ----------------
    p = dummy.Process(target=writer, args=("hello-thread",))
    p.start()
    p.join(timeout=2.0)
    assert _RESULT["value"] == "hello-thread"
    assert not p.is_alive()

    # --- multiple workers run concurrently ------------------------------
    counter = {"n": 0}
    lock = threading.Lock()

    def bump():
        for _ in range(100):
            with lock:
                counter["n"] += 1

    workers = [dummy.Process(target=bump) for _ in range(4)]
    for w in workers:
        w.start()
    for w in workers:
        w.join(timeout=5.0)
    assert counter["n"] == 400

    # --- dummy Queue / Pool round-trips ---------------------------------
    q = dummy.Queue()
    q.put(7)
    assert q.get() == 7

    with dummy.Pool(2) as pool:
        assert pool.map(lambda x: x * 2, [1, 2, 3, 4]) == [2, 4, 6, 8]

    print("multiprocessing dummy ok")


if __name__ == "__main__":
    main()
