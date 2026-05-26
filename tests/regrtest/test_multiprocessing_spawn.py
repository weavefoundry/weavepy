"""RFC 0026 — `multiprocessing` real subprocess spawn.

Exercises the full fork+exec path through the Rust
`_multiprocessing._spawn_child` helper.  Skipped on platforms (or
sandbox environments) where forking a fresh `weavepy` binary fails.
"""

import multiprocessing
import os
import sys
import tempfile
import time


def _writer(path, marker):
    with open(path, "w", encoding="utf-8") as fh:
        fh.write(marker)


def _exit_nonzero():
    sys.exit(7)


def main():
    if multiprocessing.get_start_method() != "spawn":
        multiprocessing.set_start_method("spawn", force=True)

    # ---- happy path: spawned worker writes a marker file ----------------
    with tempfile.NamedTemporaryFile(mode="w", delete=False, suffix=".marker") as fh:
        marker_path = fh.name
        fh.write("")  # ensure it exists
    try:
        p = multiprocessing.Process(target=_writer, args=(marker_path, "spawned"))
        p.start()
        assert p.pid is not None
        p.join(timeout=10.0)
        assert not p.is_alive(), "worker did not exit cleanly"
        with open(marker_path, "r", encoding="utf-8") as fh:
            assert fh.read() == "spawned", "marker missing or wrong"
    finally:
        try:
            os.unlink(marker_path)
        except OSError:
            pass

    # ---- non-zero exit code is preserved --------------------------------
    p = multiprocessing.Process(target=_exit_nonzero)
    p.start()
    p.join(timeout=10.0)
    assert p.exitcode == 7, f"expected exitcode 7, got {p.exitcode!r}"

    # ---- Pipe between the same process still works (regression guard) ---
    a, b = multiprocessing.Pipe()
    a.send({"msg": "hello", "n": 1})
    received = b.recv()
    assert received == {"msg": "hello", "n": 1}
    a.send_bytes(b"raw payload")
    assert b.recv_bytes() == b"raw payload"
    a.close()
    b.close()

    # ---- Connection is poll-able ----------------------------------------
    a, b = multiprocessing.Pipe()
    assert not a.poll(0.0)
    b.send("ping")
    deadline = time.time() + 1.0
    while not a.poll(0.05) and time.time() < deadline:
        pass
    assert a.poll(0.0)
    assert a.recv() == "ping"
    a.close()
    b.close()

    print("multiprocessing spawn ok")


if __name__ == "__main__":
    main()
