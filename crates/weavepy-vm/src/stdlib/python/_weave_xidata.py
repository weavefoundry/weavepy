"""Cross-interpreter data codec: CPython 3.14's `_PyObject_GetXIData`.

PEP 734's `_interpreters.call()`, `_interpqueues.put()`, and
`_interpchannels.send()` all convert objects with
`_PyObject_GetXIData(obj, fallback)`:

1. A *shareable* object (`None`, bool, int, float, str, bytes, tuples of
   shareables, memoryview, queues, channel ends) crosses as itself.
2. With `_PyXIDATA_FULL_FALLBACK`, a *stateless* function (no closure,
   defaults, or globals; `_PyFunction_VerifyStateless`) crosses as its
   marshalled code object and is rebuilt in the target with that
   interpreter's `__main__` namespace as `__globals__`.
3. Anything else is pickled (`_PyPickle_GetXIData`), remembering the
   sender's `__main__.__file__`: if unpickling in the target fails with
   "module '__main__' has no attribute ..." the file is run once, under
   the bogus name `<fake __main__>` (so `if __name__ == '__main__':`
   blocks stay quiet), and the unpickle is retried against that cached
   namespace (test_interpreters TestInterpreterCall
   test_func_in___main___hidden).

WeavePy's backends only move shareable values, so a converted payload is
a tuple tagged with `_TAG` that `from_xidata` unwraps on the far side.

The module also hosts the codecs `Python/crossinterp.c` exposes only
through `_testinternalcapi.get_crossinterp_data(obj, mode)`: marshal,
pickle, code, function, and script data (`get_crossinterp_data` and
`restore_crossinterp_data` below, which the Rust builtins delegate to).
Every failure surfaces as `_interpreters.NotShareableError`, with the
underlying exception as `__cause__` exactly where CPython chains one
(test_crossinterp checks both the type and the cause).
"""

import sys

import _xxsubinterpreters as _xx

XIDATA_ONLY = 0
FULL_FALLBACK = 1

_TAG = '\x00_weave_xidata\x00'
_KIND_TUPLE = 'tuple'
_KIND_FUNC = 'func'
_KIND_PICKLE = 'pickle'
_KIND_MARSHAL = 'marshal'


def _not_shareable_error():
    import _interpreters
    return _interpreters.NotShareableError


def not_shareable(msg, cause=None):
    exc = _not_shareable_error()(msg)
    if cause is not None:
        exc.__cause__ = cause
    return exc


def _unsupported(obj, cause=None):
    """`_set_xid_lookup_failure` with an object: "%R does not support..."."""
    return not_shareable(
        f'{obj!r} does not support cross-interpreter data', cause)


def is_shareable(obj):
    return _xx.is_shareable(obj)


def is_xidata(payload):
    return (type(payload) is tuple and len(payload) >= 3
            and payload[0] == _TAG)


def _verify_stateless_function(func):
    """`_PyFunction_VerifyStateless`: raise on any captured state."""
    globalsns = func.__globals__
    if globalsns is not None and not isinstance(globalsns, dict):
        raise TypeError(f'unsupported globals {globalsns!r}')
    builtinsns = func.__builtins__
    if builtinsns is not None and not isinstance(builtinsns, dict):
        raise TypeError(f'unsupported builtins {builtinsns!r}')
    if func.__defaults__:
        raise ValueError('defaults not supported')
    if func.__kwdefaults__:
        raise ValueError('keyword defaults not supported')
    if func.__closure__:
        raise ValueError('closures not supported')
    import _weave_codeinfo
    _weave_codeinfo.verify_stateless_code(
        func.__code__, None, globalsns, builtinsns)


def _mainfile():
    main = sys.modules.get('__main__')
    filename = getattr(main, '__file__', None) if main is not None else None
    return filename if isinstance(filename, str) else ''


# `_get_xidata`: the registry lookup (exact type match) plus the
# type's own "getdata" step. Only None, bool, int, float, str, bytes,
# memoryview, tuple (recursively, with the same fallback), and the
# queue/channel types are registered.
def _get_xidata(obj, fallback):
    if is_shareable(obj):
        return obj
    if type(obj) is tuple:
        # `_tuple_shared`: every item converts with the same fallback,
        # and an item's failure becomes the tuple's cause.
        items = []
        for item in obj:
            try:
                items.append(get_xidata(item, fallback))
            except _not_shareable_error() as exc:
                raise _unsupported(obj, exc) from exc
        return (_TAG, _KIND_TUPLE, tuple(items))
    if type(obj) is int:
        # `_long_shared` goes through `PyLong_AsSsize_t`, so ints past
        # C Py_ssize_t fail with OverflowError as the cause.
        cause = OverflowError('Python int too large to convert to C ssize_t')
        raise _unsupported(obj, cause) from cause
    raise _unsupported(obj)


def get_xidata(obj, fallback=FULL_FALLBACK):
    """`_PyObject_GetXIData`: convert `obj` or raise NotShareableError."""
    try:
        return _get_xidata(obj, fallback)
    except _not_shareable_error():
        if fallback != FULL_FALLBACK:
            raise
        first = sys.exception()
    import types
    if type(obj) is types.FunctionType:
        try:
            return get_function_xidata(obj)
        except _not_shareable_error():
            pass
    # We could try marshal here but CPython doesn't, for now.
    try:
        return get_pickle_xidata(obj)
    except _not_shareable_error():
        pass
    # Raise the original exception.
    raise first


# pickle

def get_pickle_xidata(obj):
    """`_PyPickle_GetXIData`: pickle `obj`, remembering `__main__.__file__`."""
    import pickle
    try:
        data = pickle.dumps(obj)
    except BaseException as exc:  # noqa: BLE001 - any pickling failure
        raise not_shareable('object could not be pickled', exc) from exc
    return (_TAG, _KIND_PICKLE, data, _mainfile())


def _running_main_namespace():
    main = sys.modules.get('__main__')
    ns = main.__dict__ if main is not None else {}
    if '__builtins__' not in ns:
        import builtins
        ns['__builtins__'] = builtins
    return ns


# Per-interpreter cache of the sender's `__main__` file loaded as a
# module namespace (CPython: `CACHED_MODULE_NS___main__` on the
# interpreter dict). Module globals are per interpreter here too.
_fake_main_cache = {}


def _missing_main_attr(exc):
    if not isinstance(exc, AttributeError):
        return False
    args = exc.args
    msg = args[0] if args and isinstance(args[0], str) else str(exc)
    return msg.startswith("module '__main__' has no attribute '")


def _isolated_main(filename):
    try:
        loaded = _fake_main_cache[filename]
    except KeyError:
        import runpy
        import types
        loaded = types.ModuleType('__main__')
        ns = runpy.run_path(filename, run_name='<fake __main__>')
        loaded.__dict__.update(ns)
        _fake_main_cache[filename] = loaded
    if isinstance(loaded, BaseException):
        raise loaded
    return loaded


def _unpickle(data, mainfile):
    import pickle
    try:
        return pickle.loads(data)
    except BaseException as exc:  # noqa: BLE001
        if not mainfile or not _missing_main_attr(exc):
            raise
        first = exc
    # Temporarily swap in the fake __main__ and retry once.
    try:
        loaded = _isolated_main(mainfile)
    except BaseException as exc:  # noqa: BLE001
        _fake_main_cache[mainfile] = exc
        raise first from None
    saved = sys.modules.get('__main__')
    sys.modules['__main__'] = loaded
    try:
        return pickle.loads(data)
    except BaseException:  # noqa: BLE001
        raise first from None
    finally:
        if saved is None:
            sys.modules.pop('__main__', None)
        else:
            sys.modules['__main__'] = saved


# marshal

def get_marshal_xidata(obj):
    """`_PyMarshal_GetXIData`: `marshal.dumps` with the current version."""
    import marshal
    try:
        data = marshal.dumps(obj, marshal.version)
    except BaseException as exc:  # noqa: BLE001
        raise not_shareable('object could not be marshalled', exc) from exc
    return (_TAG, _KIND_MARSHAL, data)


def _unmarshal(data):
    import marshal
    try:
        return marshal.loads(data)
    except BaseException as exc:  # noqa: BLE001
        raise not_shareable('object could not be unmarshalled', exc) from exc


# code

def get_code_xidata(obj):
    """`_PyCode_GetXIData`: only code objects, and they go via marshal."""
    import types
    if type(obj) is not types.CodeType:
        raise not_shareable(f'expected code, got {obj!r}')
    return get_marshal_xidata(obj)


# function

def get_function_xidata(func):
    """`_PyFunction_GetXIData`: a stateless function's marshalled code."""
    import types
    if type(func) is not types.FunctionType:
        raise not_shareable(f'expected a function, got {func!r}')
    try:
        _verify_stateless_function(func)
    except BaseException as exc:  # noqa: BLE001
        raise not_shareable(
            'only stateless functions are shareable', exc) from exc
    import marshal
    try:
        data = marshal.dumps(func.__code__, marshal.version)
    except BaseException as exc:  # noqa: BLE001
        raise not_shareable('object could not be marshalled', exc) from exc
    return (_TAG, _KIND_FUNC, data)


def _function_from_code(code):
    """`_PyFunction_FromXIData`: rebuild over the running `__main__`."""
    import types
    return types.FunctionType(code, _running_main_namespace())


# script

def _source_as_string(obj):
    """`_Py_SourceAsString`: str, bytes, bytearray, or a buffer."""
    if isinstance(obj, str):
        source = obj
    elif isinstance(obj, (bytes, bytearray)):
        source = bytes(obj)
    else:
        try:
            source = bytes(memoryview(obj))
        except TypeError:
            raise TypeError(f'unsupported script {obj!r}') from None
    nul = '\x00' if isinstance(source, str) else b'\x00'
    if nul in source:
        raise SyntaxError('source code string cannot contain null bytes')
    return source


def _verify_script(code, checked, pure):
    """`verify_script`: no closure, (if pure) no globals, no args, no value."""
    import _weave_codeinfo
    if not checked:
        builtinsns = None
        if pure:
            import builtins
            builtinsns = vars(builtins)
        _weave_codeinfo.verify_stateless_code(code, None, None, builtinsns)
    CO_VARARGS = 0x04
    CO_VARKEYWORDS = 0x08
    if (code.co_argcount > 0
            or code.co_posonlyargcount > 0
            or code.co_kwonlyargcount > 0
            or code.co_flags & (CO_VARARGS | CO_VARKEYWORDS)):
        raise ValueError('code with args not supported')
    if not _weave_codeinfo.code_returns_only_none(code):
        raise ValueError('code that returns a value is not a script')


def get_script_xidata(obj, pure=False):
    """`_PyCode_GetScriptXIData` / `_PyCode_GetPureScriptXIData`.

    A code object, a function (whose code is used), or source text
    (compiled as `<script>`) is checked to be a script: no closure, no
    arguments, and no return value. `pure` also forbids globals, and
    verifies a function's own state (`_PyFunction_VerifyStateless`).
    """
    import types
    try:
        checked = False
        if type(obj) is types.CodeType:
            code = obj
        elif type(obj) is types.FunctionType:
            code = obj.__code__
            if pure:
                _verify_stateless_function(obj)
                checked = True
        else:
            source = _source_as_string(obj)
            code = compile(source, '<script>', 'exec', 0, True, 0)
            # Compiled text can't have args or any return statements,
            # nor be a closure. It can use globals though.
            if not pure:
                checked = True
        _verify_script(code, checked, pure)
    except BaseException as exc:  # noqa: BLE001
        raise not_shareable('object not a valid script', exc) from exc
    return get_code_xidata(code)


def from_xidata(payload):
    """Rebuild the object a `get_xidata` payload stands for."""
    if not is_xidata(payload):
        return payload
    kind = payload[1]
    if kind == _KIND_TUPLE:
        return tuple(from_xidata(item) for item in payload[2])
    if kind == _KIND_FUNC:
        return _function_from_code(_unmarshal(payload[2]))
    if kind == _KIND_MARSHAL:
        return _unmarshal(payload[2])
    if kind == _KIND_PICKLE:
        try:
            return _unpickle(payload[2], payload[3])
        except BaseException as exc:  # noqa: BLE001
            raise not_shareable('object could not be unpickled', exc) from exc
    raise not_shareable(f'unknown cross-interpreter payload kind {kind!r}')


# `_testinternalcapi` entry points

class XIData:
    """The opaque handle `get_crossinterp_data` returns.

    CPython hands back a capsule around the `_PyXIData_t`; here the
    payload is the tagged tuple (or bare shareable value) the backends
    already ship, so `restore_crossinterp_data` is just `from_xidata`.
    """

    __slots__ = ('payload',)

    def __init__(self, payload):
        self.payload = payload

    def __repr__(self):
        return f'<XIData object at {id(self):#x}>'


_MODES = {
    'xidata': lambda obj: get_xidata(obj, XIDATA_ONLY),
    'fallback': lambda obj: get_xidata(obj, FULL_FALLBACK),
    'pickle': get_pickle_xidata,
    'marshal': get_marshal_xidata,
    'code': get_code_xidata,
    'func': get_function_xidata,
    'script': lambda obj: get_script_xidata(obj, pure=False),
    'script-pure': lambda obj: get_script_xidata(obj, pure=True),
}


def get_crossinterp_data(obj, mode=None):
    """`_testinternalcapi.get_crossinterp_data(obj, mode=None)`."""
    if mode is None:
        mode = 'xidata'
    elif not isinstance(mode, str):
        raise TypeError(f'expected mode str, got {mode!r}')
    elif not mode:
        mode = 'xidata'
    try:
        convert = _MODES[mode]
    except KeyError:
        raise ValueError(f'unsupported mode {mode!r}') from None
    return XIData(convert(obj))


def restore_crossinterp_data(xid):
    """`_testinternalcapi.restore_crossinterp_data(xid)`."""
    if type(xid) is not XIData:
        raise ValueError(
            'PyCapsule_GetPointer called with invalid PyCapsule object')
    return from_xidata(xid.payload)
