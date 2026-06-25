"""RFC 0040 — `multiprocessing` module surface (faithful package).

Exercises the breadth of the public API against the real CPython-style
package: synchronization primitives, `Queue`/`Pipe`, a `Pool` of real
worker processes, and a `Manager`. Because the default start method is
`spawn` (a real child re-execs and re-imports this module), the
executable body lives under an ``if __name__ == "__main__"`` guard and
`double` stays at module scope so the spawned children can import it.
"""

import multiprocessing


def double(x):
    return x * 2


def main():
    # cpu_count / current_process / active_children.
    assert isinstance(multiprocessing.cpu_count(), int)
    assert multiprocessing.cpu_count() >= 1
    assert multiprocessing.current_process().name
    assert isinstance(multiprocessing.active_children(), list)

    # Lock / RLock / Event.
    lock = multiprocessing.Lock()
    lock.acquire()
    lock.release()

    rlock = multiprocessing.RLock()
    rlock.acquire()
    rlock.release()

    event = multiprocessing.Event()
    assert not event.is_set()
    event.set()
    assert event.is_set()

    # Semaphore / BoundedSemaphore.
    sem = multiprocessing.Semaphore(2)
    sem.acquire()
    sem.release()

    bsem = multiprocessing.BoundedSemaphore(1)
    bsem.acquire()
    bsem.release()

    # Queue / Pipe.
    q = multiprocessing.Queue()
    q.put("a")
    q.put("b")
    assert q.get() == "a"
    assert q.get() == "b"

    a, b = multiprocessing.Pipe()
    a.send("hello")
    assert b.recv() == "hello"
    a.close()
    b.close()

    # Pool.map over real worker processes.
    with multiprocessing.Pool(2) as pool:
        assert pool.map(double, [1, 2, 3, 4]) == [2, 4, 6, 8]

    # Manager proxy dict.
    with multiprocessing.Manager() as m:
        d = m.dict()
        d["a"] = 1
        assert d["a"] == 1

    print("multiprocessing surface ok")


if __name__ == "__main__":
    main()
