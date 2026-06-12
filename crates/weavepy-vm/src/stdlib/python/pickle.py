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
POP = b"0"
POP_MARK = b"1"
# Memo opcodes — preserve object identity/sharing and enable cyclic
# structures. PUT/GET use a textual index (protocol 0), BINPUT/BINGET a
# 1-byte index, LONG_BINPUT/LONG_BINGET a 4-byte index, and MEMOIZE
# (protocol 4+) appends the stack top to the memo with no explicit index.
PUT = b"p"
BINPUT = b"q"
LONG_BINPUT = b"r"
GET = b"g"
BINGET = b"h"
LONG_BINGET = b"j"
MEMOIZE = b"\x94"
# Global reference + reduce opcodes used to serialize functions and
# classes by their qualified name. CPython uses these for everything
# from `pickle.dumps(int)` to `pickle.dumps(my_module.my_func)`.
GLOBAL = b"c"
STACK_GLOBAL = b"\x93"
REDUCE = b"R"
BUILD = b"b"
NEWOBJ = b"\x81"
NEWOBJ_EX = b"\x92"

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
    # CPython `Pickler.__init__`: a negative protocol selects
    # HIGHEST_PROTOCOL.
    if protocol < 0:
        protocol = HIGHEST_PROTOCOL
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


def _resolves_to_self(module, qualname, obj):
    """True when ``module.qualname`` imports back to *obj* itself.

    This is CPython's ``save_global`` self-consistency check: an object is
    only safe to pickle by reference (functions, classes, module globals)
    when the dotted name found in its declaring module *is* that object.
    Callable instances inherit their class's ``__qualname__`` and would
    otherwise be mistaken for the class.
    """
    try:
        target = __import__(module, fromlist=["_"])
        for part in qualname.split("."):
            target = getattr(target, part)
        return target is obj
    except Exception:
        return False


class _Pickler:
    def __init__(self, buf, protocol):
        self._buf = buf
        if protocol is None:
            protocol = DEFAULT_PROTOCOL
        elif protocol < 0:
            protocol = HIGHEST_PROTOCOL
        self.protocol = protocol
        self.bin = protocol >= 1
        self.fast = False
        # id(obj) -> (memo_index, obj). Keeping a reference to `obj`
        # prevents its id from being reused mid-pickle.
        self.memo = {}

    def dump(self, obj):
        self._buf.write(PROTO + bytes([self.protocol]))
        self._save(obj)
        self._buf.write(STOP)

    def _memoize(self, obj):
        """Record `obj` (already written / on the stack) in the memo and
        emit the PUT opcode so a later occurrence can reference it."""
        if self.fast or id(obj) in self.memo:
            return
        idx = len(self.memo)
        if self.protocol >= 4:
            self._buf.write(MEMOIZE)
        elif self.bin:
            if idx < 256:
                self._buf.write(BINPUT + bytes([idx]))
            else:
                self._buf.write(LONG_BINPUT + struct.pack("<I", idx))
        else:
            self._buf.write(PUT + repr(idx).encode("ascii") + b"\n")
        self.memo[id(obj)] = (idx, obj)

    def _write_get(self, idx):
        if self.bin:
            if idx < 256:
                self._buf.write(BINGET + bytes([idx]))
            else:
                self._buf.write(LONG_BINGET + struct.pack("<I", idx))
        else:
            self._buf.write(GET + repr(idx).encode("ascii") + b"\n")

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

        # Already pickled this exact object? Emit a back-reference so
        # sharing (and cycles) are preserved on load. Atomic immutables
        # (int/float) are never memoized, so they simply miss here.
        x = self.memo.get(id(obj))
        if x is not None:
            self._write_get(x[0])
            return

        # Dispatch by `type(obj).__name__` rather than `type(obj) is X`
        # or `isinstance`. WeavePy's current threading model gives each
        # worker thread its own copy of the built-in type singletons,
        # so the obvious comparisons spuriously fail when pickle is
        # invoked from a non-main thread (this matters for
        # multiprocessing.Queue's feeder threads, among others).
        tname = type(obj).__name__
        if tname == "bool":
            self._save_int(int(obj))
            return
        if tname == "int":
            self._save_int(obj)
            return
        if tname == "float":
            self._save_float(obj)
            return
        if tname == "bytes":
            self._save_bytes(obj)
            return
        if tname == "bytearray":
            # CPython reduces bytearray to `bytearray(bytes(obj))` so the
            # round-trip preserves the type (protocol < 5 form).
            self._save_reduce((bytearray, (bytes(obj),)), obj)
            return
        if tname == "str":
            self._save_str(obj)
            return
        if tname == "tuple":
            self._save_tuple(obj)
            return
        if tname == "list":
            self._save_list(obj)
            return
        if tname == "dict":
            self._save_dict(obj)
            return
        if tname == "frozenset":
            self._save_set(obj, frozen=True)
            return
        if tname == "set":
            self._save_set(obj, frozen=False)
            return
        # Functions / classes / methods — pickle them by qualified name.
        # The unpickler will `import_module(<module>); getattr(...)`.
        # We tolerate a missing `__module__` by falling back to
        # `__main__` (CPython does the same when a function is defined
        # interactively).
        try:
            is_callable_like = callable(obj)
        except Exception:
            is_callable_like = False
        try:
            # Name-based (not `isinstance`) for the same threading reason
            # as above; walk the metaclass MRO so classes with a custom
            # metaclass (`EnumType`, `ABCMeta`, …) count as types too.
            is_type = any(t.__name__ == "type" for t in type(obj).__mro__)
        except Exception:
            is_type = False
        if is_callable_like or is_type:
            module = getattr(obj, "__module__", None) or "__main__"
            qualname = (
                getattr(obj, "__qualname__", None)
                or getattr(obj, "__name__", None)
            )
            # Only pickle by name when that name actually resolves back to
            # *this* object (CPython's `save_global` self-check). A callable
            # *instance* — e.g. `operator.attrgetter('x')` — inherits its
            # class's `__qualname__`, so without this guard it would be
            # saved as the bare class and unpickle to the class object
            # rather than round-tripping through `__reduce__`.
            if qualname and _resolves_to_self(module, qualname, obj):
                self._save_global(module, qualname)
                return
            # Classes and plain/builtin functions are *only* picklable by
            # reference. CPython's `save_global` raises PicklingError for
            # anything that doesn't resolve (e.g. a class defined inside a
            # function: `<locals>` in its qualname); falling through to
            # `__reduce_ex__` here would mis-pickle the class as an
            # instance of its metaclass.
            if is_type or tname in ("function", "builtin_function_or_method"):
                raise PicklingError(
                    "Can't pickle %r: it's not found as %s.%s"
                    % (obj, module, qualname)
                )
        # Arbitrary instances — try __reduce_ex__ / __reduce__ (the
        # canonical CPython pickle protocol). Falls back to the
        # PicklingError below if neither is provided.
        # Exceptions raised by a user `__reduce_ex__` / `__reduce__`
        # propagate, as in CPython — `enum._make_class_unpicklable`
        # relies on its injected TypeError reaching the caller.
        reduce_ex = getattr(obj, "__reduce_ex__", None)
        if reduce_ex is not None:
            rv = reduce_ex(self.protocol)
            if rv is not None and rv is not NotImplemented:
                self._save_reduce(rv, obj)
                return
        reduce = getattr(obj, "__reduce__", None)
        if reduce is not None:
            rv = reduce()
            if rv is not None and rv is not NotImplemented:
                self._save_reduce(rv, obj)
                return
        raise PicklingError(
            "Can't pickle %r: pickle currently only supports primitive types"
            % obj
        )

    def _save_global(self, module, qualname):
        encoded_mod = module.encode("utf-8")
        encoded_name = qualname.encode("utf-8")
        # Protocol 4+ uses STACK_GLOBAL with two unicode strings on the
        # stack; older protocols use the textual GLOBAL opcode. We emit
        # the older form because it round-trips through any unpickler.
        self._buf.write(GLOBAL)
        self._buf.write(encoded_mod)
        self._buf.write(b"\n")
        self._buf.write(encoded_name)
        self._buf.write(b"\n")

    def _save_reduce(self, rv, obj=None):
        if isinstance(rv, str):
            self._save_global(rv.rsplit(".", 1)[0] if "." in rv else "builtins", rv)
            return
        if not isinstance(rv, tuple) or len(rv) < 2:
            raise PicklingError("reduce result must be a string or tuple")
        func = rv[0]
        args = rv[1]
        state = rv[2] if len(rv) > 2 else None
        listitems = rv[3] if len(rv) > 3 else None
        dictitems = rv[4] if len(rv) > 4 else None
        self._save(func)
        self._save(tuple(args))
        self._buf.write(REDUCE)
        # Memoize the just-constructed object *before* applying state /
        # items, so a self-referential object can back-reference itself.
        if obj is not None:
            self._memoize(obj)
        if listitems is not None:
            for item in listitems:
                self._save(item)
                self._buf.write(APPENDS[:0])  # no-op; preserves stack
        if dictitems is not None:
            for k, v in dictitems:
                self._save(k)
                self._save(v)
        if state is not None:
            self._save(state)
            self._buf.write(BUILD)

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
        self._memoize(b)

    def _save_str(self, s):
        encoded = s.encode("utf-8", "surrogatepass")
        n = len(encoded)
        if n < 256:
            self._buf.write(SHORT_BINUNICODE + bytes([n]) + encoded)
        elif n < 2**32:
            self._buf.write(BINUNICODE + struct.pack("<I", n) + encoded)
        else:
            self._buf.write(BINUNICODE8 + struct.pack("<Q", n) + encoded)
        self._memoize(s)

    def _save_tuple(self, t):
        n = len(t)
        if n == 0:
            self._buf.write(EMPTY_TUPLE)
            return
        if n <= 3:
            for item in t:
                self._save(item)
            # A nested element may have memoized *this* tuple (a cycle via
            # reduce); if so, drop what we wrote and emit the reference.
            x = self.memo.get(id(t))
            if x is not None:
                self._buf.write(POP * n)
                self._write_get(x[0])
                return
            self._buf.write((TUPLE1, TUPLE2, TUPLE3)[n - 1])
            self._memoize(t)
            return
        self._buf.write(MARK)
        for item in t:
            self._save(item)
        x = self.memo.get(id(t))
        if x is not None:
            self._buf.write(POP_MARK)
            self._write_get(x[0])
            return
        self._buf.write(TUPLE)
        self._memoize(t)

    def _save_list(self, lst):
        self._buf.write(EMPTY_LIST)
        self._memoize(lst)
        if lst:
            self._buf.write(MARK)
            for item in lst:
                self._save(item)
            self._buf.write(APPENDS)

    def _save_dict(self, d):
        self._buf.write(EMPTY_DICT)
        self._memoize(d)
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
            self._memoize(s)
        else:
            self._buf.write(EMPTY_SET)
            self._memoize(s)
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


def _pop(u):
    u.stack.pop()


def _pop_mark(u):
    idx = u.markers.pop()
    del u.stack[idx:]


def _put(u):
    idx = int(_read_line(u))
    u.memo[idx] = u.stack[-1]


def _binput(u):
    idx = u.file.read(1)[0]
    u.memo[idx] = u.stack[-1]


def _long_binput(u):
    idx = struct.unpack("<I", u._read_short(4))[0]
    u.memo[idx] = u.stack[-1]


def _memoize(u):
    u.memo[len(u.memo)] = u.stack[-1]


def _get(u):
    idx = int(_read_line(u))
    u.stack.append(u.memo[idx])


def _binget(u):
    idx = u.file.read(1)[0]
    u.stack.append(u.memo[idx])


def _long_binget(u):
    idx = struct.unpack("<I", u._read_short(4))[0]
    u.stack.append(u.memo[idx])


def _read_line(u):
    out = b""
    while True:
        ch = u.file.read(1)
        if not ch:
            raise UnpicklingError("unexpected EOF reading line")
        if ch == b"\n":
            break
        out = out + ch
    return out


def _global(u):
    module = _read_line(u).decode("utf-8")
    name = _read_line(u).decode("utf-8")
    u.stack.append(_find_class(module, name))


def _stack_global(u):
    name = u.stack.pop()
    module = u.stack.pop()
    u.stack.append(_find_class(module, name))


def _find_class(module_name, qualname):
    # `builtins` is a synthetic module name CPython uses for `len`,
    # `dict`, `Exception`, etc. WeavePy exposes those as ambient
    # globals rather than via an importable module, so route the
    # lookup through the running frame's builtins (which the VM
    # populates exactly with `default_builtins()`).
    if module_name in ("builtins", "__builtin__"):
        import builtins as _b  # may or may not exist as a module
        obj = _b
    else:
        import sys as _sys
        obj = _sys.modules.get(module_name)
        if obj is None:
            import importlib
            obj = importlib.import_module(module_name)
    for part in qualname.split("."):
        obj = getattr(obj, part)
    return obj


def _reduce(u):
    args = u.stack.pop()
    func = u.stack.pop()
    u.stack.append(func(*args))


def _build(u):
    state = u.stack.pop()
    obj = u.stack[-1]
    setstate = getattr(obj, "__setstate__", None)
    if setstate is not None:
        setstate(state)
        return
    # CPython `load_build`: a 2-tuple state is (__dict__ state, slot
    # state); apply the dict half directly and the slots via setattr.
    slotstate = None
    if isinstance(state, tuple) and len(state) == 2:
        state, slotstate = state
    if state:
        # Write the dict half straight into the instance dict, bypassing
        # `__setattr__` (CPython `load_build` does `inst.__dict__[k] = v`)
        # — this is what lets frozen dataclasses unpickle.
        d = obj.__dict__
        for k, v in state.items():
            d[k] = v
    if slotstate:
        for k, v in slotstate.items():
            setattr(obj, k, v)


def _newobj(u):
    args = u.stack.pop()
    cls = u.stack.pop()
    u.stack.append(cls.__new__(cls, *args))


def _newobj_ex(u):
    kwargs = u.stack.pop()
    args = u.stack.pop()
    cls = u.stack.pop()
    if kwargs:
        u.stack.append(cls.__new__(cls, *args, **kwargs))
    else:
        u.stack.append(cls.__new__(cls, *args))


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
    POP: _pop,
    POP_MARK: _pop_mark,
    PUT: _put,
    BINPUT: _binput,
    LONG_BINPUT: _long_binput,
    MEMOIZE: _memoize,
    GET: _get,
    BINGET: _binget,
    LONG_BINGET: _long_binget,
    GLOBAL: _global,
    STACK_GLOBAL: _stack_global,
    REDUCE: _reduce,
    BUILD: _build,
    NEWOBJ: _newobj,
    NEWOBJ_EX: _newobj_ex,
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


# CPython keeps the pure-Python implementations reachable under
# underscore names after the C-accelerator import (`pickle._dumps` is
# probed by test_descr's reduce checks). There is no separate C
# implementation here, so they alias the public functions.
_dump, _dumps, _load, _loads = dump, dumps, load, loads
