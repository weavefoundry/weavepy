"""PyArg_ParseTuple / PyArg_ParseTupleAndKeywords emulation.

A faithful Python port of CPython 3.13's ``Python/getargs.c`` argument
clinic: ``vgetargskeywords`` (keyword machinery, error wording included),
``convertsimple`` (per-unit conversion), ``converttuple`` (nested
``(...)`` groups) and ``skipitem`` (optional-argument skipping, whose
unit table must stay in sync with ``convertsimple`` — test_getargs's
SkipitemTest brute-forces the parity).  Backs the ``getargs_*`` fixture
family re-exported from the ``_testcapi`` shim.
"""

import codecs
import struct
import warnings

UCHAR_MAX = 0xFF
SHRT_MIN, SHRT_MAX = -0x8000, 0x7FFF
INT_MIN, INT_MAX = -0x80000000, 0x7FFFFFFF
LONG_MIN, LONG_MAX = -(2**63), 2**63 - 1
ULONG_MAX = 2**64 - 1
ULLONG_MAX = 2**64 - 1
SSIZE_MIN, SSIZE_MAX = LONG_MIN, LONG_MAX


class _Null:
    """C NULL: the untouched output slot of an unfilled optional unit."""

    def __repr__(self):
        return "NULL"


NULL = _Null()


class _ConvertErr(Exception):
    """convertsimple() failure: carries the `converterr` message parts."""

    def __init__(self, expected, arg):
        self.expected = expected
        self.arg = arg

    def message(self):
        if self.expected.startswith("("):
            return self.expected
        name = "None" if self.arg is None else type(self.arg).__name__
        return "must be %s, not %s" % (self.expected, name)


# ---------------------------------------------------------------------------
# Abstract-protocol helpers (PyNumber_Index / PyFloat_AsDouble /
# PyComplex_AsCComplex semantics, including the strict-subclass
# DeprecationWarnings).


def _py_index(arg):
    # `+ 0` extracts the exact int value of a subclass without
    # re-dispatching through a user __int__/__index__.
    if isinstance(arg, int):
        return arg + 0
    t = type(arg)
    f = getattr(t, "__index__", None)
    if f is None:
        raise TypeError(
            "'%s' object cannot be interpreted as an integer" % t.__name__
        )
    r = f(arg)
    if not isinstance(r, int):
        raise TypeError(
            "__index__ returned non-int (type %s)" % type(r).__name__
        )
    if type(r) is not int:
        warnings.warn(
            "__index__ returned non-int (type %s).  "
            "The ability to return an instance of a strict subclass of int "
            "is deprecated, and may be removed in a future version of Python."
            % type(r).__name__,
            DeprecationWarning,
            stacklevel=2,
        )
    return r + 0


def _as_long(arg, lo, hi, too_small, too_big):
    # PyLong_AsLong + explicit range check with getargs.c's messages.
    v = _py_index(arg)
    if v < lo:
        raise OverflowError(too_small)
    if v > hi:
        raise OverflowError(too_big)
    return v


def _as_mask(arg, mask):
    # PyLong_AsUnsignedLongMask: __index__ accepted, then wraps.
    return _py_index(arg) & mask


def _as_exact_mask(arg, mask):
    # 'k'/'K': PyLong_Check required first — no __index__ fallback.
    if not isinstance(arg, int):
        raise _ConvertErr("int", arg)
    return (arg + 0) & mask


def _as_double(arg):
    # PyFloat_AsDouble: exact/subclass float value wins outright (a
    # subclass __float__ override is never consulted, and `* 1.0`
    # preserves signed zeros where `float()` would re-dispatch).
    if isinstance(arg, float):
        return arg * 1.0
    t = type(arg)
    if isinstance(arg, int):
        return float(arg)  # int.__float__ (may raise OverflowError)
    f = getattr(t, "__float__", None)
    if f is not None:
        r = f(arg)
        if not isinstance(r, float):
            raise TypeError(
                "%s.__float__ returned non-float (type %s)"
                % (t.__name__, type(r).__name__)
            )
        if type(r) is not float:
            warnings.warn(
                "%s.__float__ returned non-float (type %s).  "
                "The ability to return an instance of a strict subclass of "
                "float is deprecated, and may be removed in a future version "
                "of Python." % (t.__name__, type(r).__name__),
                DeprecationWarning,
                stacklevel=2,
            )
        return float(r)
    return float(_py_index(arg))


def _as_complex(arg):
    # PyComplex_AsCComplex: complex (subclasses included) short-circuits
    # before __complex__ — ComplexSubclass2's override is ignored.
    if isinstance(arg, complex):
        return complex(arg.real, arg.imag)
    t = type(arg)
    f = getattr(t, "__complex__", None)
    if f is not None:
        r = f(arg)
        if not isinstance(r, complex):
            raise TypeError(
                "__complex__ returned non-complex (type %s)"
                % type(r).__name__
            )
        if type(r) is not complex:
            warnings.warn(
                "__complex__ returned non-complex (type %s).  "
                "The ability to return an instance of a strict subclass of "
                "complex is deprecated, and may be removed in a future "
                "version of Python." % type(r).__name__,
                DeprecationWarning,
                stacklevel=2,
            )
        return complex(r.real, r.imag)
    return complex(_as_double(arg), 0.0)


def _to_c_float(d):
    # C double->float cast: round-to-nearest, overflow to +/-inf
    # (struct raises where the cast saturates).
    try:
        return struct.unpack("<f", struct.pack("<f", d))[0]
    except OverflowError:
        return float("inf") if d > 0 else float("-inf")


# ---------------------------------------------------------------------------
# Buffer-protocol helpers.


def _convertbuffer(arg):
    """C `convertbuffer`: a read-only, releasebuffer-free buffer (bytes).

    Returns the raw bytes, or raises _ConvertErr with getargs.c's
    wording (bytearray/memoryview own a releasebuffer slot and are
    rejected as "read-only bytes-like object").
    """
    if isinstance(arg, bytes):
        return bytes(arg)
    if isinstance(arg, (bytearray, memoryview)):
        raise _ConvertErr("read-only bytes-like object", arg)
    raise _ConvertErr("bytes-like object", arg)


def _getbuffer(arg):
    """C `getbuffer` (PyBUF_SIMPLE): any C-contiguous buffer.

    Non-contiguous buffers raise BufferError straight through, matching
    PyObject_GetBuffer.
    """
    if isinstance(arg, bytes):
        return bytes(arg)
    if isinstance(arg, bytearray):
        return bytes(arg)
    if isinstance(arg, memoryview):
        if not arg.c_contiguous:
            raise BufferError(
                "memoryview: underlying buffer is not C-contiguous"
            )
        return bytes(arg)
    raise _ConvertErr("bytes-like object", arg)


def _get_writable_buffer(arg):
    """C 'w*' (PyBUF_WRITABLE): the object itself, byte-assignable."""
    if isinstance(arg, bytearray):
        return arg
    if isinstance(arg, memoryview):
        if arg.readonly or not arg.c_contiguous:
            # PyObject_GetBuffer failed; 'w' clears the error and
            # reports its own TypeError.
            raise _ConvertErr("read-write bytes-like object", arg)
        return arg
    raise _ConvertErr("read-write bytes-like object", arg)


def _utf8(arg):
    # PyUnicode_AsUTF8AndSize: surrogate failures propagate.
    return arg.encode("utf-8")


# ---------------------------------------------------------------------------
# The va_list stand-in: input slots consumed by 'e' units.  The
# parse_tuple_and_keywords test wrapper hands zeroed buffers to every
# slot, which read back as an empty encoding string and a NULL output
# buffer.


class _Va:
    def __init__(self, inputs=None, zeroed=False):
        self._inputs = list(inputs) if inputs else []
        self._zeroed = zeroed

    def pop_encoding(self):
        if self._inputs:
            return self._inputs.pop(0)
        if self._zeroed:
            return ""  # zeroed char buffer == empty C string
        return None  # NULL -> default encoding (utf-8)

    def pop_buffer(self):
        # ('es#'/'et#') caller-provided output buffer, or None (alloc).
        if self._inputs:
            return self._inputs.pop(0)
        return None


# ---------------------------------------------------------------------------
# Format cursor.


class _Fmt:
    def __init__(self, s):
        self.s = s
        self.i = 0

    def peek(self):
        return self.s[self.i] if self.i < len(self.s) else "\0"

    def next(self):
        c = self.peek()
        if c != "\0":
            self.i += 1
        return c

    def rest(self):
        return self.s[self.i :]

    def at_end(self):
        return self.peek() in ("\0", ":", ";")


def _seterror(iarg, msg, levels, fname=None, custom=None):
    # C `seterror`: SystemError when msg is parenthesized, else TypeError.
    if custom is not None:
        message = custom
    else:
        buf = "%s() " % fname if fname else ""
        if iarg != 0:
            buf += "argument %d" % iarg
            for lvl in levels:
                buf += ", item %d" % (lvl - 1)
        else:
            buf += "argument"
        buf += " " + msg
        message = buf
    if msg.startswith("("):
        raise SystemError(message)
    raise TypeError(message)


def _convertsimple(arg, fmt, va):
    """One unit of C `convertsimple`.  Returns the converted value.

    Raises _ConvertErr for converterr-style failures; other exceptions
    (OverflowError, ValueError, UnicodeEncodeError, BufferError, ...)
    propagate exactly as the C code leaves them set.
    """
    c = fmt.next()
    if c == "b":
        return _as_long(
            arg,
            0,
            UCHAR_MAX,
            "unsigned byte integer is less than minimum",
            "unsigned byte integer is greater than maximum",
        )
    if c == "B":
        return _as_mask(arg, 0xFF)
    if c == "h":
        return _as_long(
            arg,
            SHRT_MIN,
            SHRT_MAX,
            "signed short integer is less than minimum",
            "signed short integer is greater than maximum",
        )
    if c == "H":
        return _as_mask(arg, 0xFFFF)
    if c == "i":
        return _as_long(
            arg,
            INT_MIN,
            INT_MAX,
            "signed integer is less than minimum",
            "signed integer is greater than maximum",
        )
    if c == "I":
        return _as_mask(arg, 0xFFFFFFFF)
    if c == "n":
        v = _py_index(arg)
        if not SSIZE_MIN <= v <= SSIZE_MAX:
            raise OverflowError(
                "Python int too large to convert to C ssize_t"
            )
        return v
    if c == "l":
        v = _py_index(arg)
        if not LONG_MIN <= v <= LONG_MAX:
            raise OverflowError("Python int too large to convert to C long")
        return v
    if c == "k":
        return _as_exact_mask(arg, ULONG_MAX)
    if c == "L":
        v = _py_index(arg)
        if not LONG_MIN <= v <= LONG_MAX:
            raise OverflowError(
                "Python int too large to convert to C long long"
            )
        return v
    if c == "K":
        return _as_exact_mask(arg, ULLONG_MAX)
    if c == "f":
        return _to_c_float(_as_double(arg))
    if c == "d":
        return _as_double(arg)
    if c == "D":
        return _as_complex(arg)
    if c == "c":
        if isinstance(arg, (bytes, bytearray)) and len(arg) == 1:
            return arg[0]
        raise _ConvertErr("a byte string of length 1", arg)
    if c == "C":
        if isinstance(arg, str) and len(arg) == 1:
            return ord(arg)
        raise _ConvertErr("a unicode character", arg)
    if c == "p":
        return 1 if arg else 0
    if c == "y":
        if fmt.peek() == "*":
            fmt.next()
            return _getbuffer(arg)
        data = _convertbuffer(arg)
        if fmt.peek() == "#":
            fmt.next()
            return data
        if 0 in data:
            raise ValueError("embedded null byte")
        return data
    if c in ("s", "z"):
        if c == "z" and arg is None:
            for suffix in ("*", "#"):
                if fmt.peek() == suffix:
                    fmt.next()
                    break
            return None
        if fmt.peek() == "*":
            fmt.next()
            if isinstance(arg, str):
                return _utf8(arg)
            return _getbuffer(arg)
        if fmt.peek() == "#":
            fmt.next()
            if isinstance(arg, str):
                return _utf8(arg)
            return _convertbuffer(arg)
        if isinstance(arg, str):
            data = _utf8(arg)
            if 0 in data:
                raise ValueError("embedded null character")
            return data
        raise _ConvertErr("str or None" if c == "z" else "str", arg)
    if c == "e":
        encoding = va.pop_encoding()
        if encoding is None:
            encoding = "utf-8"
        recode = fmt.peek()
        if recode not in ("s", "t"):
            raise _ConvertErr("(unknown parser marker combination)", arg)
        fmt.next()
        if recode == "t" and isinstance(arg, (bytes, bytearray)):
            data = bytes(arg)
        elif isinstance(arg, str):
            codecs.lookup(encoding)  # LookupError for unknown/empty
            data = arg.encode(encoding)  # strict; UnicodeEncodeError raises
            if not isinstance(data, bytes):
                raise _ConvertErr("(encoding failed)", arg)
        else:
            raise _ConvertErr(
                "str" if recode == "s" else "str, bytes or bytearray", arg
            )
        if fmt.peek() == "#":
            fmt.next()
            buffer = va.pop_buffer()
            if buffer is not None:
                size = len(buffer)
                if len(data) + 1 > size:
                    raise ValueError(
                        "encoded string too long (%d, maximum length %d)"
                        % (len(data), size - 1)
                    )
                buffer[0 : len(data) + 1] = data + b"\0"
            return data
        if 0 in data:
            raise _ConvertErr("encoded string without null bytes", arg)
        return data
    if c == "S":
        if isinstance(arg, bytes):
            return arg
        raise _ConvertErr("bytes", arg)
    if c == "Y":
        if isinstance(arg, bytearray):
            return arg
        raise _ConvertErr("bytearray", arg)
    if c == "U":
        if isinstance(arg, str):
            return arg
        raise _ConvertErr("str", arg)
    if c == "O":
        # 'O!'/'O&' take C pointers; the fixtures never route them here.
        return arg
    if c == "w":
        if fmt.peek() != "*":
            raise _ConvertErr("(invalid use of 'w' format character)", arg)
        fmt.next()
        return _get_writable_buffer(arg)
    raise _ConvertErr("(impossible<bad format char>)", arg)


def _converttuple(arg, fmt, va, levels):
    """C `converttuple`: fmt is positioned after '('.

    Returns a flat tuple of the leaf values.  On converterr-style
    failure returns an error-message string (with `levels` filled);
    hard exceptions propagate.
    """
    # First pass: count top-level units of this group.
    n = 0
    depth = 0
    j = fmt.i
    s = fmt.s
    while True:
        ch = s[j] if j < len(s) else "\0"
        j += 1
        if ch == "(":
            if depth == 0:
                n += 1
            depth += 1
        elif ch == ")":
            if depth == 0:
                break
            depth -= 1
        elif ch in (":", ";", "\0"):
            break
        elif depth == 0 and ch.isalpha() and ch != "e":
            n += 1

    if not _is_sequence(arg) or isinstance(arg, bytes):
        del levels[:]
        return "must be %d-item sequence, not %s" % (
            n,
            "None" if arg is None else type(arg).__name__,
        )
    length = len(arg)
    if length != n:
        del levels[:]
        return "must be sequence of length %d, not %d" % (n, length)

    out = []
    for i in range(n):
        try:
            item = arg[i]
        except Exception:
            del levels[:]
            levels.append(i + 1)
            return "is not retrievable"
        sub_levels = []
        msg = _convertitem_inner(item, fmt, va, sub_levels, out)
        if msg is not None:
            del levels[:]
            levels.append(i + 1)
            levels.extend(sub_levels)
            return msg
    # consume the closing ')'
    assert fmt.peek() == ")"
    fmt.next()
    return tuple(out)


def _is_sequence(arg):
    # PySequence_Check: sq_item present; dicts excluded.
    if isinstance(arg, dict):
        return False
    return hasattr(type(arg), "__getitem__")


def _convertitem_inner(arg, fmt, va, levels, out):
    """C `convertitem`: appends converted leaves to `out`.

    Returns an error message string or None.
    """
    if fmt.peek() == "(":
        fmt.next()
        res = _converttuple(arg, fmt, va, levels)
        if isinstance(res, str):
            return res
        out.append(res)  # a flat-ish tuple of the group's leaves
        return None
    save = fmt.i
    try:
        out.append(_convertsimple(arg, fmt, va))
    except _ConvertErr as e:
        fmt.i = save
        del levels[:]
        return e.message()
    return None


def _flatten(value):
    if isinstance(value, tuple):
        out = []
        for v in value:
            out.extend(_flatten(v))
        return out
    return [value]


def _skipitem(fmt, va):
    """C `skipitem`.  Returns an error message string or None.

    On error the cursor is left unmoved (the caller reports the
    remaining format string).
    """
    save = fmt.i
    c = fmt.next()
    if c in "bBhHiIlkLKnfdDcCpSYU":
        return None
    if c == "e":
        va.pop_encoding()
        if fmt.peek() not in ("s", "t"):
            fmt.i = save
            return "impossible<bad format char>"
        fmt.next()
        if fmt.peek() == "#":
            fmt.next()
            va.pop_buffer()
        return None
    if c in ("s", "z", "y", "w"):
        if c == "w" and fmt.peek() != "*":
            fmt.i = save
            return "impossible<bad format char>"
        if fmt.peek() == "#":
            fmt.next()
        elif fmt.peek() == "*":
            fmt.next()
        return None
    if c == "O":
        if fmt.peek() in ("!", "&"):
            fmt.next()
        return None
    if c == "(":
        while True:
            if fmt.peek() == ")":
                break
            if fmt.at_end():
                fmt.i = save
                return "Unmatched left paren in format string"
            msg = _skipitem(fmt, va)
            if msg is not None:
                fmt.i = save
                return msg
        fmt.next()
        return None
    if c == ")":
        fmt.i = save
        return "Unmatched right paren in format string"
    fmt.i = save
    return "impossible<bad format char>"


_MISSING = object()


def _dict_get(d, name):
    """PyDict_GetItem with CPython's exact probe semantics: filter by
    the *stored* key's hash, compare with the stored key's __eq__.
    (The host dict can't be trusted for str subclasses that lie about
    __hash__/__eq__ — test_getargs's BadStr fixtures.)"""
    h = hash(name)
    for k, v in d.items():
        if k is name:
            return v
        try:
            kh = hash(k)
        except TypeError:
            continue
        if kh != h:
            continue
        if k == name:
            return v
    return _MISSING


def _kw_as_str(entry):
    # PyDict_GetItemStringRef: C strings decode strictly (errors raise).
    if isinstance(entry, bytes):
        return entry.decode("utf-8")
    return entry


def _kw_display(entry):
    # PyUnicode_FromFormat %s: utf-8 with replacement.
    if isinstance(entry, bytes):
        return entry.decode("utf-8", "replace")
    return entry


def _kw_matches(key, entry):
    # PyUnicode_EqualToUTF8: raw codepoint comparison, no user __eq__.
    try:
        key_utf8 = key.encode("utf-8", "surrogatepass")
    except UnicodeEncodeError:
        return False
    if isinstance(entry, bytes):
        return key_utf8 == entry
    return key_utf8 == entry.encode("utf-8", "surrogatepass")


def vgetargskeywords(args, kwargs, format, kwlist, va=None):
    """C `vgetargskeywords`.  Returns the per-unit results list
    (NULL sentinels mark unfilled optionals)."""
    if va is None:
        va = _Va(zeroed=True)
    args = tuple(args)
    kwargs = kwargs if kwargs is not None else {}

    fname = None
    custom_msg = None
    if ":" in format:
        fname = format[format.index(":") + 1 :]
    elif ";" in format:
        custom_msg = format[format.index(";") + 1 :]

    # positional-only prefix: leading empty keyword names
    pos = 0
    while pos < len(kwlist) and not kwlist[pos]:
        pos += 1
    for j in range(pos, len(kwlist)):
        if not kwlist[j]:
            raise SystemError("Empty keyword parameter name")
    total = len(kwlist)

    nargs = len(args)
    nkwargs = len(kwargs)
    if nargs + nkwargs > total:
        raise TypeError(
            "%s%s takes at most %d %sargument%s (%d given)"
            % (
                fname if fname else "function",
                "()" if fname else "",
                total,
                "keyword " if nargs == 0 else "",
                "" if total == 1 else "s",
                nargs + nkwargs,
            )
        )

    fmt = _Fmt(format)
    results = [NULL] * total
    min_ = None  # position of '|'
    max_ = None  # position of '$'
    skip = False
    i = 0
    while i < total:
        if fmt.peek() == "|":
            if min_ is not None:
                raise SystemError(
                    "Invalid format string (| specified twice)"
                )
            min_ = i
            fmt.next()
            if max_ is not None:
                raise SystemError("Invalid format string ($ before |)")
        if fmt.peek() == "$":
            if max_ is not None:
                raise SystemError(
                    "Invalid format string ($ specified twice)"
                )
            max_ = i
            fmt.next()
            if max_ < pos:
                raise SystemError("Empty parameter name after $")
            if skip:
                break
            if max_ < nargs:
                if max_ == 0:
                    raise TypeError(
                        "%s%s takes no positional arguments"
                        % (
                            fname if fname else "function",
                            "()" if fname else "",
                        )
                    )
                raise TypeError(
                    "%s%s takes %s %d positional argument%s (%d given)"
                    % (
                        fname if fname else "function",
                        "()" if fname else "",
                        "at most" if min_ is not None else "exactly",
                        max_,
                        "" if max_ == 1 else "s",
                        nargs,
                    )
                )
        if fmt.at_end():
            raise SystemError(
                "More keyword list entries (%d) than format specifiers (%d)"
                % (total, i)
            )
        if not skip:
            current_arg = NULL
            if i < nargs:
                current_arg = args[i]
            elif nkwargs and i >= pos:
                key = _kw_as_str(kwlist[i])  # may raise UnicodeDecodeError
                found = _dict_get(kwargs, key)
                if found is not _MISSING:
                    current_arg = found
                    nkwargs -= 1

            if current_arg is not NULL:
                levels = []
                out = []
                msg = _convertitem_inner(current_arg, fmt, va, levels, out)
                if msg is not None:
                    _seterror(i + 1, msg, levels, fname, custom_msg)
                results[i] = out[0]
                i += 1
                continue

            if min_ is None or i < min_:
                if i < pos:
                    assert min_ is None
                    assert max_ is None
                    skip = True
                    # exact bounds unknown yet; error deferred to '|'/'$'
                    # or end of format
                else:
                    raise TypeError(
                        "%s%s missing required argument '%s' (pos %d)"
                        % (
                            fname if fname else "function",
                            "()" if fname else "",
                            _kw_display(kwlist[i]),
                            i + 1,
                        )
                    )
            if not nkwargs and not skip:
                return results

        msg = _skipitem(fmt, va)
        if msg is not None:
            raise SystemError("%s: '%s'" % (msg, fmt.rest()))
        i += 1

    if skip:
        bound = min(pos, min_ if min_ is not None else total)
        raise TypeError(
            "%s%s takes %s %d positional argument%s (%d given)"
            % (
                fname if fname else "function",
                "()" if fname else "",
                "at least" if bound < i else "exactly",
                bound,
                "" if bound == 1 else "s",
                nargs,
            )
        )

    if not fmt.at_end() and fmt.peek() not in ("|", "$"):
        raise SystemError(
            "more argument specifiers than keyword list entries "
            "(remaining format:'%s')" % fmt.rest()
        )

    if nkwargs > 0:
        # arguments given by name and position?
        for j in range(pos, nargs):
            key = _kw_as_str(kwlist[j])
            if _dict_get(kwargs, key) is not _MISSING:
                raise TypeError(
                    "argument for %s%s given by name ('%s') "
                    "and position (%d)"
                    % (
                        fname if fname else "function",
                        "()" if fname else "",
                        key,
                        j + 1,
                    )
                )
        # extraneous keyword arguments?
        for key in kwargs:
            if not isinstance(key, str):
                raise TypeError("keywords must be strings")
            match = False
            for j in range(pos, total):
                if _kw_matches(key, kwlist[j]):
                    match = True
                    break
            if not match:
                raise TypeError(
                    "%s%s got an unexpected keyword argument '%s'"
                    % (
                        fname if fname else "this function",
                        "()" if fname else "",
                        str(key),
                    )
                )
        # keys char-match keyword names yet dict lookup missed them
        # (hash/__eq__ liars): CPython's catch-all.
        raise TypeError(
            "invalid keyword argument for %s%s"
            % (fname if fname else "this function", "()" if fname else "")
        )

    return results


def parse_tuple(args, format, va=None):
    """C `vgetargs1` (positional-only PyArg_ParseTuple).

    Returns the per-unit results list.  Only the subset our fixtures
    need: min/max via '|', unit conversion, count errors.
    """
    if va is None:
        va = _Va()
    args = tuple(args)
    fname = None
    custom_msg = None
    if ":" in format:
        fname = format[format.index(":") + 1 :]
    elif ";" in format:
        custom_msg = format[format.index(";") + 1 :]

    # count top-level units and the '|' position
    fmt_scan = _Fmt(format)
    min_ = None
    max_ = 0
    depth = 0
    while not fmt_scan.at_end():
        ch = fmt_scan.next()
        if ch == "(":
            if depth == 0:
                max_ += 1
            depth += 1
        elif ch == ")":
            depth -= 1
        elif ch == "|" and depth == 0:
            min_ = max_
        elif depth == 0 and ch.isalpha() and ch != "e":
            # 'e' itself is skipped; its trailing s/t carries the count
            # (matches vgetargs1_impl's unit counting).
            max_ += 1
    if min_ is None:
        min_ = max_

    nargs = len(args)
    if nargs < min_ or nargs > max_:
        if min_ == max_:
            quant = "exactly %d" % min_
        elif nargs < min_:
            quant = "at least %d" % min_
        else:
            quant = "at most %d" % max_
        raise TypeError(
            "%s%s takes %s argument%s (%d given)"
            % (
                fname if fname else "function",
                "()" if fname else "",
                quant,
                "" if quant.endswith(" 1") else "s",
                nargs,
            )
        )

    fmt = _Fmt(format)
    results = []
    for i in range(nargs):
        if fmt.peek() == "|":
            fmt.next()
        levels = []
        out = []
        msg = _convertitem_inner(args[i], fmt, va, levels, out)
        if msg is not None:
            _seterror(i + 1, msg, levels, fname, custom_msg)
        results.append(out[0])
    return results


def parse_one(arg, format, va=None):
    """C `PyArg_Parse` single-argument compat form ('es'/'et' fixtures)."""
    if va is None:
        va = _Va()
    fname = None
    custom_msg = None
    if ":" in format:
        fname = format[format.index(":") + 1 :]
    elif ";" in format:
        custom_msg = format[format.index(";") + 1 :]
    fmt = _Fmt(format)
    levels = []
    out = []
    msg = _convertitem_inner(arg, fmt, va, levels, out)
    if msg is not None:
        _seterror(0, msg, levels, fname, custom_msg)
    return out[0]
