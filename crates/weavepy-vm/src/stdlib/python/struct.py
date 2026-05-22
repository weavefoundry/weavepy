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
_iter_unpack_impl = _wrap(_impl.iter_unpack)


def iter_unpack(fmt, buffer):
    """Iterate over `buffer` in `calcsize(fmt)` chunks."""
    items = _iter_unpack_impl(fmt, buffer)
    return iter(items)


class Struct:
    """Pre-compiled binary format. Mirrors `struct.Struct`."""

    __slots__ = ("format", "size", "_fmt")

    def __init__(self, fmt):
        if isinstance(fmt, bytes):
            fmt = fmt.decode("ascii")
        self.format = fmt
        self._fmt = fmt
        self.size = calcsize(fmt)

    def pack(self, *values):
        return pack(self._fmt, *values)

    def unpack(self, buffer):
        return unpack(self._fmt, buffer)

    def pack_into(self, buffer, offset, *values):
        return pack_into(self._fmt, buffer, offset, *values)

    def unpack_from(self, buffer, offset=0):
        return unpack_from(self._fmt, buffer, offset)

    def iter_unpack(self, buffer):
        return iter_unpack(self._fmt, buffer)


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
