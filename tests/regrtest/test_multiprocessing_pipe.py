"""RFC 0026 — multiprocessing.Pipe over a real socketpair.

Covers send / recv (pickle round-trip), send_bytes / recv_bytes,
poll(timeout), close(), and the BrokenPipeError surface.
"""

import multiprocessing
import time


def main():
    # --- pickled round-trip of a heterogeneous payload -------------------
    a, b = multiprocessing.Pipe()
    a.send([1, 2, 3])
    assert b.recv() == [1, 2, 3]
    a.send({"x": 1, "y": (4, 5)})
    assert b.recv() == {"x": 1, "y": (4, 5)}
    a.send(None)
    assert b.recv() is None
    a.send("hello")
    assert b.recv() == "hello"

    # --- raw bytes round-trip --------------------------------------------
    a.send_bytes(b"opaque")
    assert b.recv_bytes() == b"opaque"
    a.send_bytes(b"hello world", offset=6, size=5)
    assert b.recv_bytes() == b"world"

    # --- poll() with timeout ---------------------------------------------
    assert not b.poll(0.0)
    a.send("ping")
    start = time.time()
    assert b.poll(1.0)
    assert (time.time() - start) < 0.5
    assert b.recv() == "ping"

    # --- close() makes subsequent recv raise EOF / BrokenPipe ------------
    a.send("last")
    a.close()
    assert b.recv() == "last"
    try:
        a.send("after close")
    except OSError:
        pass
    else:
        raise AssertionError("send after close should raise")
    try:
        b.recv()
    except (EOFError, OSError):
        pass
    else:
        raise AssertionError("recv after EOF should raise")
    b.close()

    # --- duplex pipe: both ends bidirectional ----------------------------
    a, b = multiprocessing.Pipe(duplex=True)
    a.send("from a")
    b.send("from b")
    assert b.recv() == "from a"
    assert a.recv() == "from b"
    a.close()
    b.close()

    print("multiprocessing pipe ok")


if __name__ == "__main__":
    main()
