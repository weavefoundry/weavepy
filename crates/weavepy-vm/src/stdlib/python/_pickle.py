"""WeavePy's `_pickle` — the "C accelerator" lane built on the pure
implementation.

CPython's `_pickle` is a C rewrite of `pickle.py`; `pickle.py` imports it
for the fast paths and `test_pickle` runs its matrix against both lanes.
WeavePy's pickle *is* the pure-Python implementation, so this module
re-exports it under the accelerator's names — with one real difference
kept faithful: the C module's *error discipline*. Where the pure engine
leaks `IndexError` / `struct.error` on malformed input and tolerates
sloppy `__reduce__` values, the C one raises `UnpicklingError` /
`PicklingError`, and `test_pickle`'s C lanes assert exactly that
(`CUnpicklerTests.bad_stack_errors == (UnpicklingError,)`). The
subclasses below reproduce that discipline; identity probes
(`pickle.Pickler is pickle._Pickler`) keep distinguishing the lanes just
as on CPython.

Import order is self-untangling. `import pickle` first: pickle's early
`from _pickle import PickleBuffer` starts this module, whose
`from pickle import …` below can't resolve yet (PickleError isn't
defined at that point), so it raises ImportError, pickle falls back to
its pure definitions, and pickle's *final* `from _pickle import …`
re-imports this module successfully. `import _pickle` first: the
`from pickle import …` below fully initializes pickle (its own
`from _pickle import …` attempts see this half-built module and fall
back), then resolves. Either way both modules share one set of classes.
"""

import io as _io
import sys as _sys
from struct import unpack as _unpack

from pickle import (
    PickleError,
    PicklingError,
    UnpicklingError,
    PickleBuffer,
    HIGHEST_PROTOCOL,
    DEFAULT_PROTOCOL,
    MARK as _MARK,
    FRAME as _FRAME,
    UNICODE as _UNICODE,
    LIST as _LIST,
    PERSID as _PERSID,
    _Pickler,
    _Unpickler,
    _Unframer,
    _Stop,
)

__all__ = [
    "PickleError",
    "PicklingError",
    "UnpicklingError",
    "PickleBuffer",
    "Pickler",
    "Unpickler",
    "dump",
    "dumps",
    "load",
    "loads",
]


class Pickler(_Pickler):
    """The accelerator Pickler: the pure engine plus the C module's
    `save_reduce` argument validation (`Modules/_pickle.c:save_reduce`):
    `__newobj_ex__` args must be `(cls, tuple, dict)`, the
    listitems/dictitems elements must be *iterators* (the pure engine
    tolerates any iterable), and a state setter must be callable. Like
    the C object, a Pickler mid-`dump` rejects reentrant `dump` /
    `__init__` calls with RuntimeError instead of corrupting its state
    (test_concurrent_pickler_dump*)."""

    _weavepy_active = False

    # The C accelerator's Argument Clinic signature — all four are
    # positional-or-keyword there (the pure engine makes fix_imports/
    # buffer_callback keyword-only); inspect.signature(_pickle.Pickler)
    # must render it exactly (test_inspect test_signature_on_builtin_class).
    def __init__(self, file, protocol=None, fix_imports=True,
                 buffer_callback=None):
        if self._weavepy_active:
            raise RuntimeError(
                "Pickler.__init__() called while a dump is in progress")
        _Pickler.__init__(self, file, protocol, fix_imports=fix_imports,
                          buffer_callback=buffer_callback)

    def dump(self, obj):
        if self._weavepy_active:
            raise RuntimeError("Pickler already in use by another dump")
        self._weavepy_active = True
        try:
            return _Pickler.dump(self, obj)
        finally:
            self._weavepy_active = False

    def save_reduce(self, func, args, state=None, listitems=None,
                    dictitems=None, state_setter=None, *, obj=None):
        if getattr(func, "__name__", "") == "__newobj_ex__" and \
                isinstance(args, tuple) and len(args) == 3:
            _, newargs, kwargs = args
            if not isinstance(newargs, tuple):
                raise PicklingError(
                    "second item from __newobj_ex__ args is not a tuple, "
                    "not %s" % type(newargs).__name__)
            if not isinstance(kwargs, dict):
                raise PicklingError(
                    "third item from __newobj_ex__ args is not a dict, "
                    "not %s" % type(kwargs).__name__)
        if listitems is not None and not hasattr(type(listitems), "__next__"):
            raise PicklingError(
                "fourth element of the tuple returned by __reduce__ "
                "must be an iterator, not %s" % type(listitems).__name__)
        if dictitems is not None and not hasattr(type(dictitems), "__next__"):
            raise PicklingError(
                "fifth element of the tuple returned by __reduce__ "
                "must be an iterator, not %s" % type(dictitems).__name__)
        if state_setter is not None and not callable(state_setter):
            raise PicklingError(
                "sixth element of the tuple returned by __reduce__ "
                "must be a function, not %s" % type(state_setter).__name__)
        return _Pickler.save_reduce(self, func, args, state, listitems,
                                    dictitems, state_setter, obj=obj)


class _BoundBuiltinMethod:
    """A bound `_MethodDescriptor`: the shim analogue of a bound C
    method. The `__class__` lie makes `inspect.isbuiltin` hold, which
    is what pydoc's `_is_bound_method` checks before rendering
    "method of _pickle.Pickler instance" (test_pydoc
    test_bound_builtin_method)."""

    __slots__ = ("_descr", "__self__")

    def __init__(self, descr, obj):
        self._descr = descr
        self.__self__ = obj

    @property
    def __class__(self):
        import types
        return types.BuiltinFunctionType

    @property
    def __name__(self):
        return self._descr.__name__

    @property
    def __qualname__(self):
        return self._descr.__qualname__

    @property
    def __doc__(self):
        return self._descr.__doc__

    @property
    def __text_signature__(self):
        return self._descr.__text_signature__

    @property
    def __func__(self):
        return self._descr

    def __call__(self, *args, **kwargs):
        return self._descr._func(self.__self__, *args, **kwargs)

    def __repr__(self):
        return "<built-in method %s of %s object at %#x>" % (
            self.__name__, type(self.__self__).__name__, id(self.__self__))


# The C accelerator's `method_descriptor` face for a shim method:
# `inspect.ismethoddescriptor` is true (a `__get__` with no `__set__`
# on the type), and `__objclass__`/`__text_signature__` feed pydoc's
# "dump(self, obj, /) unbound _pickle.Pickler method" summary
# (test_pydoc test_unbound_builtin_method). No class docstring —
# `__doc__` must stay a slot carrying the wrapped function's doc.
class _MethodDescriptor:
    __slots__ = ("_func", "__objclass__", "__text_signature__",
                 "__name__", "__qualname__", "__doc__")

    def __init__(self, func, objclass, text_signature):
        self._func = func
        self.__objclass__ = objclass
        self.__text_signature__ = text_signature
        self.__name__ = func.__name__
        self.__qualname__ = "%s.%s" % (objclass.__name__, func.__name__)
        self.__doc__ = func.__doc__

    @property
    def __module__(self):
        return "_pickle"

    def __get__(self, obj, objtype=None):
        if obj is None:
            return self
        return _BoundBuiltinMethod(self, obj)

    def __call__(self, *args, **kwargs):
        return self._func(*args, **kwargs)

    def __repr__(self):
        return "<method '%s' of '_pickle.%s' objects>" % (
            self.__name__, self.__objclass__.__name__)


# `Pickler.dump` is a `method_descriptor` in the C module; rebind the
# plain function through the shim descriptor so introspection matches.
Pickler.dump = _MethodDescriptor(
    Pickler.__dict__["dump"], Pickler, "($self, obj, /)")


class _StrictStack(list):
    """An unpickling stack whose underflows surface as the C
    accelerator's `UnpicklingError` instead of `IndexError`."""

    def pop(self, index=-1):
        try:
            return list.pop(self, index)
        except IndexError:
            raise UnpicklingError("unpickling stack underflow") from None

    def __getitem__(self, index):
        try:
            return list.__getitem__(self, index)
        except IndexError:
            raise UnpicklingError("unpickling stack underflow") from None


def _strict_load_mark(self):
    # `_Unpickler.load_mark` swaps in a *plain* list; keep the strict
    # container so later underflows on the new stack stay UnpicklingError.
    self.metastack.append(self.stack)
    self.stack = _StrictStack()
    self.append = self.stack.append


def _strict_load_frame(self):
    # The pure `_Unframer.load_frame` buffers whatever bytes are left
    # without checking the declared size; the C module notices a short
    # frame (test_truncated_data's b'\x95\x02\0\0\0\0\0\0\0' cases).
    frame_size, = _unpack('<Q', self.read(8))
    if frame_size > _sys.maxsize:
        raise ValueError("frame size > sys.maxsize: %d" % frame_size)
    self._unframer.load_frame(frame_size)
    frame = self._unframer.current_frame
    if frame is not None and frame.getbuffer().nbytes < frame_size:
        raise UnpicklingError("pickle exhausted before end of frame")


def _strict_load_list(self):
    # LIST appends the popped mark slice *itself* as the unpickled object;
    # every other mark consumer only reads from the slice (where the strict
    # container's UnpicklingError-on-underflow is wanted). Convert here so
    # `_StrictStack` never leaks into results. NB: `pop_mark` rebinds
    # `self.append`, so it must run before the append lookup — never
    # `self.append(list(self.pop_mark()))`.
    items = self.pop_mark()
    self.append(list(items))


def _strict_load_unicode(self):
    # Protocol-0 UNICODE is a line-reading opcode whose payload may
    # legitimately be empty (`V\n` is the empty string): C's
    # `load_unicode` checks `len < 1` where most line readers check
    # `len < 2`.
    data = self._raw_readline()
    if not data.endswith(b"\n"):
        raise UnpicklingError("pickle data was truncated")
    self.append(str(data[:-1], "raw-unicode-escape"))


def _strict_load_persid(self):
    # PERSID likewise allows an empty line: a falsy persistent id such
    # as "" is legal (C `load_persid` checks `len < 1`;
    # AbstractPersistentPicklerTests pickles "test_false_value" as pid "").
    data = self._raw_readline()
    if not data.endswith(b"\n"):
        raise UnpicklingError("pickle data was truncated")
    try:
        pid = data[:-1].decode("ascii")
    except UnicodeDecodeError:
        raise UnpicklingError(
            "persistent IDs in protocol 0 must be ASCII strings")
    self.append(self.persistent_load(pid))


class Unpickler(_Unpickler):
    """The accelerator Unpickler: the pure engine wrapped in the C
    module's error discipline (`Modules/_pickle.c`):

    * a truncated opcode argument raises
      `UnpicklingError("pickle data was truncated")` (C `bad_readline`),
      including text lines missing their newline or shorter than one
      character plus newline;
    * stack/metastack underflow on malformed opcode sequences raises
      `UnpicklingError("unpickling stack underflow")`;
    * a FRAME shorter than its declared size raises `UnpicklingError`;
    * running out of input *between* opcodes stays `EOFError` ("Ran out
      of input"), exactly like the C main loop.

    Everything else — `find_class` import errors, `UnicodeDecodeError`
    from bad module names, `ValueError` from bad protocol/extension
    codes, exceptions raised by reconstructed objects — propagates
    unchanged, as it does from the C implementation.

    Like the C object, an Unpickler mid-`load` rejects reentrant `load`
    / `__init__` calls with RuntimeError, and its `memo` attribute
    validates assignment (dict with non-negative integer keys).
    """

    _weavepy_active = False

    def __init__(self, *args, **kwargs):
        if self._weavepy_active:
            raise RuntimeError(
                "Unpickler.__init__() called while a load is in progress")
        _Unpickler.__init__(self, *args, **kwargs)

    @property
    def memo(self):
        return self._weavepy_memo

    @memo.setter
    def memo(self, value):
        # C `Unpickler_set_memo`: a dict (or another unpickler's memo
        # proxy, which materializes as a dict here) whose keys are
        # non-negative integers.
        if not isinstance(value, dict):
            raise TypeError("'memo' attribute must be a dict")
        for key in value:
            if not isinstance(key, int):
                raise TypeError("memo key must be integers")
            if key < 0:
                raise ValueError("memo key must be positive integers.")
        self._weavepy_memo = value

    def load(self):
        # Mirrors `_Unpickler.load` (which we can't call directly: the
        # opcode-fetch read must stay permissive while argument reads
        # turn strict).
        if self._weavepy_active:
            raise RuntimeError("Unpickler already in use by another load")
        if not hasattr(self, "_file_read"):
            raise UnpicklingError(
                "Unpickler.__init__() was not called by %s.__init__()"
                % (self.__class__.__name__,)
            )
        self._unframer = _Unframer(self._file_read, self._file_readline)
        raw_read = self._unframer.read
        raw_readline = self._unframer.readline

        def read(n):
            data = raw_read(n)
            if len(data) < n:
                raise UnpicklingError("pickle data was truncated")
            return data

        def readline():
            data = raw_readline()
            if len(data) < 2 or not data.endswith(b"\n"):
                raise UnpicklingError("pickle data was truncated")
            return data

        def readinto(buf):
            # The pure `_Unframer.readinto` silently accepts a short
            # fill; route through the strict read instead.
            data = read(len(buf))
            buf[:] = data
            return len(data)

        self.read = read
        self.readline = readline
        self.readinto = readinto
        self._raw_readline = raw_readline
        self.metastack = _StrictStack()
        self.stack = _StrictStack()
        self.append = self.stack.append
        self.proto = 0
        dispatch = self.dispatch
        self._weavepy_active = True
        try:
            while True:
                key = raw_read(1)
                if not key:
                    raise EOFError("Ran out of input")
                dispatch[key[0]](self)
        except _Stop as stopinst:
            return stopinst.value
        finally:
            self._weavepy_active = False

    # A fresh dispatch table so the strict opcode variants apply without
    # disturbing `pickle._Unpickler`.
    dispatch = dict(_Unpickler.dispatch)
    dispatch[_MARK[0]] = _strict_load_mark
    dispatch[_FRAME[0]] = _strict_load_frame
    dispatch[_UNICODE[0]] = _strict_load_unicode
    dispatch[_PERSID[0]] = _strict_load_persid
    dispatch[_LIST[0]] = _strict_load_list


# A callable that does *not* implement the descriptor protocol, mirroring
# how a C-level `builtin_function_or_method` behaves as a class attribute:
# `class T: loads = _pickle.loads` must leave `T.loads` unbound
# (test_pickle's `CPickleTests` does exactly that with a class-body
# `from _pickle import dump, dumps, load, loads`). No class docstring —
# `__doc__` must stay a slot so each wrapper carries its function's doc.
class _BuiltinFunction:
    __slots__ = ("_func", "__name__", "__qualname__", "__doc__")

    def __init__(self, func):
        self._func = func
        self.__name__ = func.__name__
        self.__qualname__ = func.__qualname__
        self.__doc__ = func.__doc__

    @property
    def __module__(self):
        return "_pickle"

    def __call__(self, *args, **kwargs):
        return self._func(*args, **kwargs)

    def __repr__(self):
        return "<built-in function %s>" % (self.__name__,)


@_BuiltinFunction
def dump(obj, file, protocol=None, *, fix_imports=True, buffer_callback=None):
    Pickler(file, protocol, fix_imports=fix_imports,
            buffer_callback=buffer_callback).dump(obj)


@_BuiltinFunction
def dumps(obj, protocol=None, *, fix_imports=True, buffer_callback=None):
    f = _io.BytesIO()
    Pickler(f, protocol, fix_imports=fix_imports,
            buffer_callback=buffer_callback).dump(obj)
    res = f.getvalue()
    assert isinstance(res, (bytes, bytearray))
    return res


@_BuiltinFunction
def load(file, *, fix_imports=True, encoding="ASCII", errors="strict",
         buffers=None):
    return Unpickler(file, fix_imports=fix_imports, buffers=buffers,
                     encoding=encoding, errors=errors).load()


@_BuiltinFunction
def loads(s, /, *, fix_imports=True, encoding="ASCII", errors="strict",
          buffers=None):
    if isinstance(s, str):
        raise TypeError("Can't load pickle from unicode string")
    file = _io.BytesIO(s)
    return Unpickler(file, fix_imports=fix_imports, buffers=buffers,
                     encoding=encoding, errors=errors).load()
