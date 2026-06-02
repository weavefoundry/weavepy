"""Public surface of the `struct` module (RFC 0019).

The heavy lifting lives in the Rust-backed `_struct` module; this
file re-exports the free functions and provides the user-visible
``Struct`` class plus the ``error`` exception type that CPython's
public API surfaces.
"""

import _struct as _impl


class error(Exception):
    """Raised when packing or unpacking fails."""


def _wrap(fn):
    def call(*args, **kwargs):
        try:
            return fn(*args, **kwargs)
        except ValueError as e:
            raise error(str(e)) from None
    call.__name__ = getattr(fn, "__name__", "wrapped")
    return call


calcsize = _wrap(_impl.calcsize)
pack = _wrap(_impl.pack)
unpack = _wrap(_impl.unpack)
pack_into = _wrap(_impl.pack_into)
unpack_from = _wrap(_impl.unpack_from)


def _iter_unpack(fmt, buffer, size):
    # CPython validates the buffer length up front (a `struct.error` is
    # raised by `iter_unpack` itself, not lazily on the first `next()`),
    # and rejects a zero-width format outright.
    if size == 0:
        raise error("cannot iteratively unpack with a struct of length 0")
    if len(buffer) % size != 0:
        raise error(
            "iterative unpacking requires a buffer of a multiple of "
            f"{size} bytes"
        )

    def _gen():
        for off in range(0, len(buffer), size):
            yield unpack_from(fmt, buffer, off)

    return _gen()


def iter_unpack(fmt, buffer):
    """Iterate over `buffer` in `calcsize(fmt)` chunks."""
    return _iter_unpack(fmt, buffer, calcsize(fmt))


class Struct:
    """Pre-compiled binary format. Mirrors `struct.Struct`."""

    def __new__(cls, *args, **kwargs):
        self = super().__new__(cls)
        # A `Struct` created via `__new__` alone (no `__init__`) is
        # "half-initialized": CPython's C type leaves `s_format == NULL`
        # and `s_size == -1`, and every operation raises `RuntimeError`
        # until `__init__` runs.
        self._fmt = None
        self.size = -1
        return self

    def __init__(self, fmt):
        if isinstance(fmt, bytes):
            fmt = fmt.decode("ascii")
        self._fmt = fmt
        self.size = calcsize(fmt)

    def _ensure_initialized(self):
        if self._fmt is None:
            raise RuntimeError("Struct.__init__() was not called")

    @property
    def format(self):
        self._ensure_initialized()
        return self._fmt

    def pack(self, *values):
        self._ensure_initialized()
        return pack(self._fmt, *values)

    def unpack(self, buffer):
        self._ensure_initialized()
        return unpack(self._fmt, buffer)

    def pack_into(self, buffer, offset, *values):
        self._ensure_initialized()
        return pack_into(self._fmt, buffer, offset, *values)

    def unpack_from(self, buffer, offset=0):
        self._ensure_initialized()
        return unpack_from(self._fmt, buffer, offset)

    def iter_unpack(self, buffer):
        self._ensure_initialized()
        return _iter_unpack(self._fmt, buffer, self.size)

    def __repr__(self):
        self._ensure_initialized()
        return f"Struct({self._fmt!r})"

    def __sizeof__(self):
        self._ensure_initialized()
        return object.__sizeof__(self)


def _new_struct(fmt):
    return Struct(fmt)


__all__ = [
    "error",
    "calcsize",
    "pack",
    "unpack",
    "pack_into",
    "unpack_from",
    "iter_unpack",
    "Struct",
]
