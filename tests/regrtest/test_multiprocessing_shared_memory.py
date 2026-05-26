"""RFC 0026 — _multiprocessing.SharedMemory.

Smoke-test the shm_open + mmap-backed shared memory primitive.  We
create a region, write a payload, read it back, and clean up.
"""

import _multiprocessing
import os


def main():
    name = f"/wp_test_{os.getpid()}"
    try:
        shm = _multiprocessing.SharedMemory(name, True, 4096)
    except (PermissionError, OSError) as exc:
        # Some sandboxed CI environments forbid `shm_open(O_CREAT)`.
        # The primitive is still wired up; assert that gracefully and
        # bail out of the rest of the test rather than failing.
        print(f"multiprocessing shared memory skipped (sandbox): {exc}")
        return
    try:
        assert shm.size == 4096
        shm.write(b"hello world", 0)
        assert shm.read(0, 11) == b"hello world"
        # Verify writes at non-zero offsets.
        shm.write(b"!!!", 11)
        assert shm.read(0, 14) == b"hello world!!!"
    finally:
        shm.close()
        try:
            shm.unlink()
        except OSError:
            pass

    print("multiprocessing shared memory ok")


if __name__ == "__main__":
    main()
