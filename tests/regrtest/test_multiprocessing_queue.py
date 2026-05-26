"""RFC 0026 — multiprocessing.Queue and JoinableQueue.

Multi-producer / multi-consumer correctness with threads (the
cross-process path is exercised by `test_multiprocessing_spawn` and
the CPython `test_multiprocessing_*` suites).
"""

import multiprocessing
import threading
import time


def main():
    # --- single-threaded round-trip --------------------------------------
    q = multiprocessing.Queue()
    q.put(1)
    q.put("two")
    q.put({"three": 3})
    assert q.get() == 1
    assert q.get() == "two"
    assert q.get() == {"three": 3}

    # --- empty() guard ---------------------------------------------------
    assert q.empty()

    # --- get_nowait raises ----------------------------------------------
    try:
        import queue as _q
        q.get_nowait()
    except _q.Empty:
        pass
    else:
        raise AssertionError("get_nowait on empty queue should raise")

    # --- multi-producer + multi-consumer via threads --------------------
    q = multiprocessing.Queue()
    received = []
    lock = threading.Lock()

    def producer(start):
        for i in range(start, start + 10):
            q.put(i)

    def consumer():
        for _ in range(10):
            v = q.get(timeout=5.0)
            with lock:
                received.append(v)

    threads = [
        threading.Thread(target=producer, args=(0,)),
        threading.Thread(target=producer, args=(100,)),
        threading.Thread(target=consumer),
        threading.Thread(target=consumer),
    ]
    for t in threads:
        t.start()
    for t in threads:
        t.join(timeout=10.0)

    assert sorted(received) == sorted(list(range(10)) + list(range(100, 110)))

    # --- JoinableQueue: task_done + join sequence ------------------------
    jq = multiprocessing.JoinableQueue()
    for i in range(5):
        jq.put(i)

    drained = []

    def worker():
        while True:
            try:
                item = jq.get(timeout=0.5)
            except Exception:
                break
            drained.append(item)
            jq.task_done()

    t = threading.Thread(target=worker)
    t.start()
    jq.join()
    t.join(timeout=2.0)
    assert sorted(drained) == [0, 1, 2, 3, 4]

    print("multiprocessing queue ok")


if __name__ == "__main__":
    main()
