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
"""

import sys

import _xxsubinterpreters as _xx

XIDATA_ONLY = 0
FULL_FALLBACK = 1

_TAG = '\x00_weave_xidata\x00'
_KIND_TUPLE = 'tuple'
_KIND_FUNC = 'func'
_KIND_PICKLE = 'pickle'


def _not_shareable_error():
    import _interpreters
    return _interpreters.NotShareableError


def not_shareable(msg, cause=None):
    exc = _not_shareable_error()(msg)
    if cause is not None:
        exc.__cause__ = cause
    return exc


def is_shareable(obj):
    return _xx.is_shareable(obj)


def is_xidata(payload):
    return (type(payload) is tuple and len(payload) >= 3
            and payload[0] == _TAG)


def _verify_stateless_function(func):
    """`_PyFunction_VerifyStateless`: raise on any captured state."""
    if func.__defaults__:
        raise ValueError('defaults not supported')
    if func.__kwdefaults__:
        raise ValueError('keyword defaults not supported')
    if func.__closure__:
        raise ValueError('closures not supported')
    import builtins
    import _weave_codeinfo
    builtinsns = func.__builtins__
    if not isinstance(builtinsns, dict):
        builtinsns = vars(builtins)
    _weave_codeinfo.verify_stateless_code(
        func.__code__, None, func.__globals__, builtinsns)


def _mainfile():
    main = sys.modules.get('__main__')
    filename = getattr(main, '__file__', None) if main is not None else None
    return filename if isinstance(filename, str) else ''


def get_xidata(obj, fallback=FULL_FALLBACK):
    """Convert `obj` to a shareable payload, or raise NotShareableError."""
    if is_shareable(obj):
        return obj
    if type(obj) is tuple:
        # `_tuple_shared`: every item converts with the same fallback.
        return (_TAG, _KIND_TUPLE,
                tuple(get_xidata(item, fallback) for item in obj))
    if fallback != FULL_FALLBACK:
        raise not_shareable(
            f'{type(obj).__name__} does not support cross-interpreter data')
    import types
    if type(obj) is types.FunctionType:
        try:
            _verify_stateless_function(obj)
        except Exception:
            pass
        else:
            import marshal
            return (_TAG, _KIND_FUNC, marshal.dumps(obj.__code__))
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


def from_xidata(payload):
    """Rebuild the object a `get_xidata` payload stands for."""
    if not is_xidata(payload):
        return payload
    kind = payload[1]
    if kind == _KIND_TUPLE:
        return tuple(from_xidata(item) for item in payload[2])
    if kind == _KIND_FUNC:
        import marshal
        import types
        code = marshal.loads(payload[2])
        return types.FunctionType(code, _running_main_namespace())
    if kind == _KIND_PICKLE:
        try:
            return _unpickle(payload[2], payload[3])
        except BaseException as exc:  # noqa: BLE001
            raise not_shareable('object could not be unpickled', exc) from exc
    raise not_shareable(f'unknown cross-interpreter payload kind {kind!r}')
