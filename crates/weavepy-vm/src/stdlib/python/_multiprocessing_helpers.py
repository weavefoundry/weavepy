"""Private helpers used by the frozen ``multiprocessing`` package.

These live in a separate module so that ``multiprocessing`` itself can
remain small and import-driven; tests and the test runner can also
import them directly to exercise edge cases.
"""

import os
import sys
import _multiprocessing as _mp


_DEFAULT_START_METHOD = "fork" if sys.platform != "win32" else "spawn"
_VALID_START_METHODS = ("fork", "spawn", "forkserver")


def get_default_start_method():
    return _DEFAULT_START_METHOD


def validate_start_method(method):
    if method not in _VALID_START_METHODS:
        raise ValueError(
            f"start method {method!r} is not one of {_VALID_START_METHODS!r}")


def child_argv(payload_fd):
    """Construct the ``sys.argv`` a multiprocessing worker child should run with."""
    exe = sys.executable or "weavepy"
    return [exe, "--multiprocessing-fork", str(payload_fd)]


def get_executable():
    return sys.executable or "weavepy"


def get_command_line():
    """Return the argv prefix used by spawn/forkserver children."""
    return child_argv("PAYLOAD_FD_PLACEHOLDER")


def get_temp_dir():
    candidates = [
        os.environ.get("TMPDIR"),
        "/tmp",
        os.environ.get("TEMP"),
        os.environ.get("TMP"),
    ]
    for c in candidates:
        if c and os.path.isdir(c):
            return c
    return "."


def remove_path_safely(path):
    try:
        os.unlink(path)
    except FileNotFoundError:
        pass
    except OSError:
        pass


def write_int(fd, n):
    """Write a 64-bit little-endian unsigned int to a file descriptor."""
    data = int(n).to_bytes(8, "little", signed=False)
    pos = 0
    while pos < len(data):
        wrote = os.write(fd, data[pos:])
        if wrote == 0:
            raise OSError("write() returned 0")
        pos += wrote


def read_int(fd):
    """Read a 64-bit little-endian unsigned int from a file descriptor."""
    data = b""
    while len(data) < 8:
        chunk = os.read(fd, 8 - len(data))
        if not chunk:
            raise EOFError("EOF while reading int")
        data += chunk
    return int.from_bytes(data, "little", signed=False)


def write_payload(fd, payload):
    """Write a length-prefixed ``bytes`` payload to *fd*."""
    if not isinstance(payload, (bytes, bytearray)):
        raise TypeError("payload must be bytes")
    write_int(fd, len(payload))
    pos = 0
    while pos < len(payload):
        wrote = os.write(fd, payload[pos:])
        if wrote == 0:
            raise OSError("write() returned 0")
        pos += wrote


def read_payload(fd):
    """Read a length-prefixed ``bytes`` payload from *fd*."""
    n = read_int(fd)
    out = bytearray()
    while len(out) < n:
        chunk = os.read(fd, n - len(out))
        if not chunk:
            raise EOFError("EOF while reading payload")
        out += chunk
    return bytes(out)


def fd_writable(fd):
    """Best-effort check that *fd* is open for writing."""
    try:
        os.fstat(fd)
        return True
    except OSError:
        return False


def close_fds(keep=()):
    """Close every file descriptor not in *keep*. Best-effort."""
    keep = set(int(f) for f in keep)
    try:
        soft_limit, _ = (256, 256)
        import resource as _r
        soft_limit, _ = _r.getrlimit(_r.RLIMIT_NOFILE)
    except Exception:
        soft_limit = 1024
    for fd in range(3, soft_limit):
        if fd in keep:
            continue
        try:
            os.close(fd)
        except OSError:
            pass


# Re-export the raw Rust core for convenience.
SemLock = getattr(_mp, "SemLock", None)
Connection = getattr(_mp, "Connection", None)
shared_pipe = getattr(_mp, "shared_pipe", None)
shared_socketpair = getattr(_mp, "shared_socketpair", None)
shared_memory_create = getattr(_mp, "shared_memory_create", None)
shared_memory_attach = getattr(_mp, "shared_memory_attach", None)
shared_memory_unlink = getattr(_mp, "shared_memory_unlink", None)
shared_memory_close = getattr(_mp, "shared_memory_close", None)
sem_unlink = getattr(_mp, "sem_unlink", None)
