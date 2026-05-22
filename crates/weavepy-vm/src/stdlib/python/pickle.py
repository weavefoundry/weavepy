"""Public ``pickle`` module (RFC 0019).

A pure-Python implementation of the CPython pickle protocol that
faithfully matches the on-the-wire byte sequences for the
universally-supported subset (None / bool / int / float / bytes /
str / tuple / list / dict / set / frozenset / nested combinations).

Object/class pickling is *not* exposed here — that requires
``__reduce__`` plumbing through the type system which lives in a
later RFC. Calling ``pickle.dumps`` on an arbitrary object will
raise ``PicklingError`` instead of silently producing a bytestream
that cannot be loaded.
"""

import io
import struct

HIGHEST_PROTOCOL = 5
DEFAULT_PROTOCOL = 5

# --- opcodes (the subset we emit) -----------------------------------------
PROTO = b"\x80"
FRAME = b"\x95"
EMPTY_DICT = b"}"
EMPTY_LIST = b"]"
EMPTY_TUPLE = b")"
EMPTY_SET = b"\x8f"
NONE = b"N"
NEWTRUE = b"\x88"
NEWFALSE = b"\x89"
BININT = b"J"
BININT1 = b"K"
BININT2 = b"M"
LONG1 = b"\x8a"
LONG4 = b"\x8b"
BINFLOAT = b"G"
BINUNICODE = b"X"
SHORT_BINUNICODE = b"\x8c"
BINUNICODE8 = b"\x8d"
BINBYTES = b"B"
SHORT_BINBYTES = b"C"
BINBYTES8 = b"\x8e"
SETITEMS = b"u"
APPENDS = b"e"
TUPLE1 = b"\x85"
TUPLE2 = b"\x86"
TUPLE3 = b"\x87"
TUPLE = b"t"
ADDITEMS = b"\x90"
FROZENSET = b"\x91"
MARK = b"("
STOP = b"."

# --- exceptions -----------------------------------------------------------


class PickleError(Exception):
    pass


class PicklingError(PickleError):
    pass


class UnpicklingError(PickleError):
    pass


# --- public entry points --------------------------------------------------


def dumps(obj, protocol=None, *, fix_imports=True, buffer_callback=None):
    if protocol is None:
        protocol = DEFAULT_PROTOCOL
    if not 0 <= protocol <= HIGHEST_PROTOCOL:
        raise ValueError("unsupported pickle protocol: %d" % protocol)
    pickler = _Pickler(io.BytesIO(), protocol)
    pickler.dump(obj)
    return pickler._buf.getvalue()


def dump(obj, file, protocol=None, *, fix_imports=True, buffer_callback=None):
    file.write(dumps(obj, protocol, fix_imports=fix_imports,
                     buffer_callback=buffer_callback))


def loads(data, *, fix_imports=True, encoding="ASCII", errors="strict"):
    return _Unpickler(io.BytesIO(data)).load()


def load(file, *, fix_imports=True, encoding="ASCII", errors="strict"):
    return _Unpickler(file).load()


# --- pickler --------------------------------------------------------------


class _Pickler:
    def __init__(self, buf, protocol):
        self._buf = buf
        self.protocol = protocol
        self.bin = protocol >= 1
        self.fast = False

    def dump(self, obj):
        self._buf.write(PROTO + bytes([self.protocol]))
        self._save(obj)
        self._buf.write(STOP)

    def _save(self, obj):
        # In the order CPython tries dispatch:
        # 1. None
        if obj is None:
            self._buf.write(NONE)
            return
        if obj is True:
            self._buf.write(NEWTRUE)
            return
        if obj is False:
            self._buf.write(NEWFALSE)
            return

        t = type(obj)
        if t is int:
            self._save_int(obj)
            return
        if t is float:
            self._save_float(obj)
            return
        if t is bytes:
            self._save_bytes(obj)
            return
        if t is bytearray:
            self._save_bytes(bytes(obj))
            return
        if t is str:
            self._save_str(obj)
            return
        if t is tuple:
            self._save_tuple(obj)
            return
        if t is list:
            self._save_list(obj)
            return
        if t is dict:
            self._save_dict(obj)
            return
        if t is set:
            self._save_set(obj, frozen=False)
            return
        if t is frozenset:
            self._save_set(obj, frozen=True)
            return
        raise PicklingError(
            "Can't pickle %r: pickle currently only supports primitive types"
            % obj
        )

    def _save_int(self, n):
        if 0 <= n < 256:
            self._buf.write(BININT1 + bytes([n]))
            return
        if 0 <= n < 65536:
            self._buf.write(BININT2 + struct.pack("<H", n))
            return
        if -2**31 <= n < 2**31:
            self._buf.write(BININT + struct.pack("<i", n))
            return
        # LONG1 / LONG4 with two's-complement encoding.
        if n == 0:
            self._buf.write(LONG1 + b"\x00")
            return
        nbits = n.bit_length() + 1
        nbytes = (nbits + 7) // 8
        encoded = n.to_bytes(nbytes, "little", True)
        if nbytes < 256:
            self._buf.write(LONG1 + bytes([nbytes]) + encoded)
        else:
            self._buf.write(LONG4 + struct.pack("<i", nbytes) + encoded)

    def _save_float(self, x):
        self._buf.write(BINFLOAT + struct.pack(">d", x))

    def _save_bytes(self, b):
        n = len(b)
        if n < 256:
            self._buf.write(SHORT_BINBYTES + bytes([n]) + b)
        elif n < 2**32:
            self._buf.write(BINBYTES + struct.pack("<I", n) + b)
        else:
            self._buf.write(BINBYTES8 + struct.pack("<Q", n) + b)

    def _save_str(self, s):
        encoded = s.encode("utf-8", "surrogatepass")
        n = len(encoded)
        if n < 256:
            self._buf.write(SHORT_BINUNICODE + bytes([n]) + encoded)
        elif n < 2**32:
            self._buf.write(BINUNICODE + struct.pack("<I", n) + encoded)
        else:
            self._buf.write(BINUNICODE8 + struct.pack("<Q", n) + encoded)

    def _save_tuple(self, t):
        n = len(t)
        if n == 0:
            self._buf.write(EMPTY_TUPLE)
            return
        if n == 1:
            self._save(t[0])
            self._buf.write(TUPLE1)
            return
        if n == 2:
            self._save(t[0])
            self._save(t[1])
            self._buf.write(TUPLE2)
            return
        if n == 3:
            for item in t:
                self._save(item)
            self._buf.write(TUPLE3)
            return
        self._buf.write(MARK)
        for item in t:
            self._save(item)
        self._buf.write(TUPLE)

    def _save_list(self, lst):
        self._buf.write(EMPTY_LIST)
        if lst:
            self._buf.write(MARK)
            for item in lst:
                self._save(item)
            self._buf.write(APPENDS)

    def _save_dict(self, d):
        self._buf.write(EMPTY_DICT)
        if d:
            self._buf.write(MARK)
            for k, v in d.items():
                self._save(k)
                self._save(v)
            self._buf.write(SETITEMS)

    def _save_set(self, s, frozen):
        items = list(s)
        if frozen:
            # FROZENSET pops a MARK and consumes everything above it.
            self._buf.write(MARK)
            for it in items:
                self._save(it)
            self._buf.write(FROZENSET)
        else:
            self._buf.write(EMPTY_SET)
            if items:
                self._buf.write(MARK)
                for it in items:
                    self._save(it)
                self._buf.write(ADDITEMS)


# --- unpickler ------------------------------------------------------------


class _Unpickler:
    def __init__(self, file):
        self.file = file
        self.stack = []
        self.markers = []
        self.memo = {}

    def load(self):
        while True:
            op = self.file.read(1)
            if not op:
                raise UnpicklingError("pickle data was truncated")
            handler = _OPCODES.get(op)
            if handler is None:
                raise UnpicklingError(
                    "unsupported opcode %r at offset %d" % (op, self.file.tell()))
            result = handler(self)
            if result is _STOP:
                return self.stack.pop()

    def _read_short(self, n):
        return self.file.read(n)

    def _pop_to_mark(self):
        idx = self.markers.pop()
        items = []
        while len(self.stack) > idx:
            items.append(self.stack.pop())
        items.reverse()
        return items


_STOP = object()


def _none(u):
    u.stack.append(None)


def _newtrue(u):
    u.stack.append(True)


def _newfalse(u):
    u.stack.append(False)


def _binint(u):
    u.stack.append(struct.unpack("<i", u._read_short(4))[0])


def _binint1(u):
    u.stack.append(u.file.read(1)[0])


def _binint2(u):
    u.stack.append(struct.unpack("<H", u._read_short(2))[0])


def _long1(u):
    n = u.file.read(1)[0]
    if n == 0:
        u.stack.append(0)
        return
    data = u.file.read(n)
    u.stack.append(int.from_bytes(data, "little", True))


def _long4(u):
    n = struct.unpack("<i", u._read_short(4))[0]
    data = u.file.read(n)
    u.stack.append(int.from_bytes(data, "little", True))


def _binfloat(u):
    u.stack.append(struct.unpack(">d", u._read_short(8))[0])


def _binbytes(u):
    n = struct.unpack("<I", u._read_short(4))[0]
    u.stack.append(u.file.read(n))


def _short_binbytes(u):
    n = u.file.read(1)[0]
    u.stack.append(u.file.read(n))


def _binbytes8(u):
    n = struct.unpack("<Q", u._read_short(8))[0]
    u.stack.append(u.file.read(n))


def _binunicode(u):
    n = struct.unpack("<I", u._read_short(4))[0]
    u.stack.append(u.file.read(n).decode("utf-8", "surrogatepass"))


def _short_binunicode(u):
    n = u.file.read(1)[0]
    u.stack.append(u.file.read(n).decode("utf-8", "surrogatepass"))


def _binunicode8(u):
    n = struct.unpack("<Q", u._read_short(8))[0]
    u.stack.append(u.file.read(n).decode("utf-8", "surrogatepass"))


def _empty_tuple(u):
    u.stack.append(())


def _tuple1(u):
    a = u.stack.pop()
    u.stack.append((a,))


def _tuple2(u):
    b = u.stack.pop()
    a = u.stack.pop()
    u.stack.append((a, b))


def _tuple3(u):
    c = u.stack.pop()
    b = u.stack.pop()
    a = u.stack.pop()
    u.stack.append((a, b, c))


def _tuple_op(u):
    items = u._pop_to_mark()
    u.stack.append(tuple(items))


def _empty_list(u):
    u.stack.append([])


def _appends(u):
    items = u._pop_to_mark()
    u.stack[-1].extend(items)


def _empty_dict(u):
    u.stack.append({})


def _setitems(u):
    items = u._pop_to_mark()
    d = u.stack[-1]
    for i in range(0, len(items), 2):
        d[items[i]] = items[i + 1]


def _empty_set(u):
    u.stack.append(set())


def _additems(u):
    items = u._pop_to_mark()
    s = u.stack[-1]
    for i in items:
        s.add(i)


def _frozenset(u):
    items = u._pop_to_mark()
    u.stack.append(frozenset(items))


def _mark(u):
    u.markers.append(len(u.stack))


def _proto(u):
    u.file.read(1)


def _frame(u):
    u._read_short(8)


def _stop(_u):
    return _STOP


_OPCODES = {
    PROTO: _proto,
    FRAME: _frame,
    NONE: _none,
    NEWTRUE: _newtrue,
    NEWFALSE: _newfalse,
    BININT: _binint,
    BININT1: _binint1,
    BININT2: _binint2,
    LONG1: _long1,
    LONG4: _long4,
    BINFLOAT: _binfloat,
    BINBYTES: _binbytes,
    SHORT_BINBYTES: _short_binbytes,
    BINBYTES8: _binbytes8,
    BINUNICODE: _binunicode,
    SHORT_BINUNICODE: _short_binunicode,
    BINUNICODE8: _binunicode8,
    EMPTY_TUPLE: _empty_tuple,
    TUPLE1: _tuple1,
    TUPLE2: _tuple2,
    TUPLE3: _tuple3,
    TUPLE: _tuple_op,
    EMPTY_LIST: _empty_list,
    APPENDS: _appends,
    EMPTY_DICT: _empty_dict,
    SETITEMS: _setitems,
    EMPTY_SET: _empty_set,
    ADDITEMS: _additems,
    FROZENSET: _frozenset,
    MARK: _mark,
    STOP: _stop,
}


__all__ = ["dump", "dumps", "load", "loads",
           "Pickler", "Unpickler",
           "PickleError", "PicklingError", "UnpicklingError",
           "DEFAULT_PROTOCOL", "HIGHEST_PROTOCOL"]


# Public class aliases for code that does ``pickle.Pickler(f).dump(obj)``.
class Pickler(_Pickler):
    def __init__(self, file, protocol=None, *, fix_imports=True,
                 buffer_callback=None):
        super().__init__(file, protocol if protocol is not None else DEFAULT_PROTOCOL)


class Unpickler(_Unpickler):
    pass
