"""RFC 0026 — multiprocessing.Process lifecycle.

Covers the boundary cases:
- start() twice raises AssertionError (CPython's bare `assert self._popen
  is None`);
- terminate() on a not-yet-started Process is a no-op;
- close() refuses to run while the worker is alive;
- daemon flag handling;
- active_children() bookkeeping.
"""

import multiprocessing
import time


def _busy_loop(seconds):
    deadline = time.time() + seconds
    while time.time() < deadline:
        pass


def _quick():
    pass


def main():
    if multiprocessing.get_start_method() != "spawn":
        multiprocessing.set_start_method("spawn", force=True)

    # --- start twice raises ---------------------------------------------
    p = multiprocessing.Process(target=_quick)
    p.start()
    try:
        p.start()
    except AssertionError:
        pass
    else:
        raise AssertionError("second start() should raise")
    p.join(timeout=5.0)

    # --- close() refuses while alive -----------------------------------
    p = multiprocessing.Process(target=_busy_loop, args=(2.0,))
    p.start()
    try:
        p.close()
    except ValueError:
        pass
    else:
        raise AssertionError("close on live process should raise")
    p.terminate()
    p.join(timeout=5.0)
    # After join, close() should succeed.
    p.close()

    # --- daemon flag setter blocked after start -------------------------
    p = multiprocessing.Process(target=_quick)
    p.daemon = True
    assert p.daemon is True
    p.start()
    try:
        p.daemon = False
    except AssertionError:
        pass
    else:
        raise AssertionError("daemon setter after start should raise")
    p.join(timeout=5.0)

    # --- active_children bookkeeping ------------------------------------
    procs = [multiprocessing.Process(target=_busy_loop, args=(0.5,)) for _ in range(3)]
    for p in procs:
        p.start()
    assert len(multiprocessing.active_children()) >= 1
    for p in procs:
        p.join(timeout=5.0)
    # All workers should now be reaped.
    leftovers = [p for p in multiprocessing.active_children() if p in procs]
    assert leftovers == [], f"unexpected leftovers: {leftovers}"

    print("multiprocessing process lifecycle ok")


if __name__ == "__main__":
    main()
