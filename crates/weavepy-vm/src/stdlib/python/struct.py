"""Public surface of the `struct` module (RFC 0019).

The heavy lifting lives in the Rust-backed `_struct` module; this
file re-exports the free functions and provides the user-visible
``Struct`` class plus the ``error`` exception type that CPython's
public API surfaces.
"""

import operator as _operator

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


_INT_CODES = frozenset("bBhHiIlLqQnNP")
_FLOAT_CODES = frozenset("fde")


def _coerce_values(fmt, values):
    """Coerce each argument through the protocol its format code implies.

    The Rust ``_struct`` core only sees concrete ``int``/``float``/``bool``
    objects, but CPython's ``struct`` runs ``__index__`` on integer codes,
    ``__float__`` on float codes, and ``__bool__`` on ``?`` (so e.g. an
    object whose ``__bool__`` raises propagates that exception). Mirror
    that here, where we have interpreter access.
    """
    try:
        codes = _impl._value_codes(fmt)
    except ValueError as e:
        raise error(str(e)) from None
    if len(codes) != len(values):
        # Let the core raise the canonical "pack expected N items" error.
        return values
    out = []
    for code, v in zip(codes, values):
        if code in _INT_CODES:
            if isinstance(v, bool):
                out.append(int(v))
            elif isinstance(v, int):
                out.append(v)
            else:
                try:
                    out.append(_operator.index(v))
                except (TypeError, AttributeError):
                    raise error("required argument is not an integer") from None
        elif code in _FLOAT_CODES:
            if isinstance(v, float):
                out.append(v)
            else:
                try:
                    out.append(float(v))
                except (TypeError, ValueError):
                    raise error("required argument is not a float") from None
        elif code == "?":
            out.append(bool(v))
        else:
            out.append(v)
    return out


def _readable(buffer):
    """Return a bytes-like view of `buffer` the Rust core understands.

    The `_struct` core reads `bytes`/`bytearray`/`memoryview` directly. Any
    other object implementing the buffer protocol (notably `array.array`)
    is surfaced through its `tobytes()` export — CPython accepts any
    buffer-protocol object here, and this is the slice of that protocol we
    can reach from the frozen wrapper (test_struct.test_unpack_with_buffer).
    """
    if isinstance(buffer, (bytes, bytearray, memoryview)):
        return buffer
    tobytes = getattr(buffer, "tobytes", None)
    if callable(tobytes):
        return tobytes()
    # Let the core raise the canonical "a bytes-like object is required".
    return buffer


calcsize = _wrap(_impl.calcsize)


def unpack(fmt, buffer):
    try:
        return _impl.unpack(fmt, _readable(buffer))
    except ValueError as e:
        raise error(str(e)) from None


def unpack_from(fmt, buffer, offset=0):
    try:
        return _impl.unpack_from(fmt, _readable(buffer), offset)
    except ValueError as e:
        raise error(str(e)) from None


def pack(fmt, *values):
    values = _coerce_values(fmt, values)
    try:
        return _impl.pack(fmt, *values)
    except ValueError as e:
        raise error(str(e)) from None


def _writable(buffer):
    """Resolve `buffer` to a read-write target the Rust core can pack into.

    CPython's `pack_into` requires a writable buffer-protocol object. We
    accept `bytearray` and writable `memoryview` directly, take any other
    buffer-protocol object (e.g. `array.array`) through `memoryview()`, and
    reject read-only / non-buffer arguments with `TypeError`
    (test_struct.test_pack_into).
    """
    if isinstance(buffer, bytearray):
        return buffer
    if isinstance(buffer, memoryview):
        if buffer.readonly:
            raise TypeError("cannot modify read-only memory")
        return buffer
    if isinstance(buffer, (bytes, str)):
        raise TypeError(
            "argument must be a read-write bytes-like object, not "
            + type(buffer).__name__
        )
    mv = memoryview(buffer)  # raises TypeError if no buffer protocol
    if mv.readonly:
        raise TypeError("argument must be a read-write bytes-like object")
    return mv


def pack_into(fmt, buffer, offset, *values):
    target = _writable(buffer)
    values = _coerce_values(fmt, values)
    try:
        return _impl.pack_into(fmt, target, offset, *values)
    except ValueError as e:
        raise error(str(e)) from None


class unpack_iterator:
    """Iterator returned by `iter_unpack` / `Struct.iter_unpack`.

    Mirrors CPython's `unpack_iterator` C type: it can't be constructed
    directly from Python, it yields one tuple per `size`-byte chunk, and
    `__length_hint__` reports the number of chunks still to come (so
    `operator.length_hint` and list-preallocation behave as CPython's do —
    test_struct.test_length_hint / test_uninstantiable).
    """

    __slots__ = ("_fmt", "_buffer", "_size", "_offset", "_len")

    def __new__(cls, *args, **kwargs):
        raise TypeError("cannot create 'unpack_iterator' instances")

    def __iter__(self):
        return self

    def __next__(self):
        if self._offset >= self._len:
            raise StopIteration
        result = unpack_from(self._fmt, self._buffer, self._offset)
        self._offset += self._size
        return result

    def __length_hint__(self):
        return (self._len - self._offset) // self._size


def _make_unpack_iterator(fmt, buffer, size):
    # CPython validates the buffer length up front (a `struct.error` is
    # raised by `iter_unpack` itself, not lazily on the first `next()`),
    # and rejects a zero-width format outright.
    if size == 0:
        raise error("cannot iteratively unpack with a struct of length 0")
    buffer = _readable(buffer)
    if len(buffer) % size != 0:
        raise error(
            "iterative unpacking requires a buffer of a multiple of "
            f"{size} bytes"
        )
    # Bypass the guard `__new__` to build the (otherwise unconstructable)
    # iterator, exactly as the C type does internally.
    it = object.__new__(unpack_iterator)
    it._fmt = fmt
    it._buffer = buffer
    it._size = size
    it._offset = 0
    it._len = len(buffer)
    return it


def iter_unpack(fmt, buffer):
    """Iterate over `buffer` in `calcsize(fmt)` chunks."""
    return _make_unpack_iterator(fmt, buffer, calcsize(fmt))


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
        # CPython encodes the format to a C string, so a non-ASCII format
        # (e.g. a lone surrogate) raises UnicodeEncodeError, and an invalid
        # but ASCII format raises struct.error. Both must be detected
        # *before* we mutate `self`, so a failed re-`__init__` leaves the
        # previously-compiled format intact (test_Struct_reinitialization).
        if isinstance(fmt, str):
            fmt.encode("ascii")  # validates encodability; may raise UnicodeEncodeError
        elif isinstance(fmt, (bytes, bytearray)):
            fmt = bytes(fmt).decode("ascii")
        size = calcsize(fmt)
        self._fmt = fmt
        self.size = size

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
        return _make_unpack_iterator(self._fmt, buffer, self.size)

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
