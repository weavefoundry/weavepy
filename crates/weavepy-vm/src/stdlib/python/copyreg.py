"""Public ``copyreg`` module — pickle/copy registry helpers (RFC 0019).

This is a faithful port of CPython's ``copyreg`` module surface:
``constructor``, ``pickle``, ``__reduce_ex__`` defaults, and the
private dispatch tables that ``pickle.dumps`` uses to learn how a
type should be serialised.
"""

dispatch_table = {}


def pickle(ob_type, pickle_function, constructor_ob=None):
    if not callable(pickle_function):
        raise TypeError("reduction functions must be callable")
    dispatch_table[ob_type] = pickle_function
    if constructor_ob is not None:
        constructor(constructor_ob)


def constructor(object):
    if not callable(object):
        raise TypeError("constructors must be callable")
    _safe_constructors[id(object)] = object


_safe_constructors = {}


def _reconstructor(cls, base, state):
    if base is object:
        obj = object.__new__(cls)
    else:
        obj = base.__new__(cls, state)
    if base.__init__ != object.__init__:
        base.__init__(obj, state)
    return obj


_HEAPTYPE = 1 << 9
_new_type = type(int)


def __newobj__(cls, *args):
    return cls.__new__(cls, *args)


def __newobj_ex__(cls, args, kwargs):
    return cls.__new__(cls, *args, **kwargs)


def _default_getstate(obj):
    """The CPython 3.11+ ``object.__getstate__`` default.

    Returns the instance ``__dict__`` (or ``None`` when empty), folded
    together with any ``__slots__`` state as a ``(dict_state, slot_state)``
    pair when slots carry values.
    """
    try:
        d = obj.__dict__
    except AttributeError:
        d = None
    dict_state = d if d else None
    slot_state = None
    names = _slotnames(type(obj))
    if names:
        slot_state = {}
        for name in names:
            try:
                slot_state[name] = getattr(obj, name)
            except AttributeError:
                pass
        if not slot_state:
            slot_state = None
    if slot_state is not None:
        return (dict_state, slot_state)
    return dict_state


def _bytearray_reduce(obj, protocol):
    """CPython ``bytearray.__reduce_ex__`` (``_common_reduce`` in
    Objects/bytearrayobject.c): the buffer content rides as a
    constructor argument so ``cls(content)`` rebuilds the payload, and
    the instance state (``__dict__`` / slots) follows separately."""
    getstate = getattr(obj, "__getstate__", None)
    if getstate is not None:
        state = getstate()
    else:
        state = _default_getstate(obj)
    if protocol < 3:
        # str-based reduction (Python 2.x compatible), like CPython.
        return (type(obj), (bytes(obj).decode('latin-1'), 'latin-1'), state)
    return (type(obj), (bytes(obj),), state)


def _reduce_newobj(obj, protocol):
    """Port of CPython's ``object.__reduce_ex__`` protocol-2+ path
    (``Objects/typeobject.c:reduce_newobj``).

    Produces the ``(callable, args, state, listitems, dictitems)`` tuple
    that ``copy``/``pickle`` feed to ``copyreg._reconstruct`` to rebuild
    the instance, honouring the ``__getnewargs_ex__`` / ``__getnewargs__``
    and ``__getstate__`` hooks.
    """
    cls = type(obj)
    getnewargs_ex = getattr(obj, "__getnewargs_ex__", None)
    if getnewargs_ex is not None:
        args, kwargs = getnewargs_ex()
    else:
        getnewargs = getattr(obj, "__getnewargs__", None)
        args = getnewargs() if getnewargs is not None else ()
        kwargs = {}

    if kwargs:
        newobj = __newobj_ex__
        newargs = (cls, tuple(args), kwargs)
    else:
        newobj = __newobj__
        newargs = (cls,) + tuple(args)

    getstate = getattr(obj, "__getstate__", None)
    if getstate is not None:
        state = getstate()
    else:
        state = _default_getstate(obj)

    listitems = iter(obj) if isinstance(obj, list) else None
    dictitems = iter(obj.items()) if isinstance(obj, dict) else None
    return newobj, newargs, state, listitems, dictitems


def _slotnames(cls):
    """Return a (possibly cached) list of slot-style attribute names."""
    slotnames = cls.__dict__.get("__slotnames__")
    if slotnames is not None:
        return slotnames
    if not isinstance(cls, type):
        raise TypeError("_slotnames() requires a type")
    names = []
    if not hasattr(cls, "__slots__"):
        slotnames = []
    else:
        for c in cls.__mro__:
            if "__slots__" in c.__dict__:
                slots = c.__dict__["__slots__"]
                if isinstance(slots, str):
                    slots = [slots]
                for name in slots:
                    if name in ("__dict__", "__weakref__"):
                        continue
                    # mangled names — but a class named only with
                    # underscores (e.g. ``___``) strips to "" and the
                    # slot keeps its raw name (CPython parity).
                    if name.startswith("__") and not name.endswith("__"):
                        stripped = c.__name__.lstrip("_")
                        if stripped:
                            names.append("_%s%s" % (stripped, name))
                        else:
                            names.append(name)
                    else:
                        names.append(name)
        slotnames = names
    try:
        cls.__slotnames__ = slotnames
    except TypeError:
        pass
    return slotnames


# A registry of extension codes (ad-hoc pickle compression). Codes are
# positive ints in [1, 0x7fffffff]; 0 is reserved. These tables are a
# faithful port of CPython's copyreg extension registry — pickle grabs a
# reference at init, so the names must never be rebound.
_extension_registry = {}                # key -> code
_inverted_registry = {}                 # code -> key
_extension_cache = {}                   # code -> object


def add_extension(module, name, code):
    """Register an extension code."""
    code = int(code)
    if not 1 <= code <= 0x7fffffff:
        raise ValueError("code out of range")
    key = (module, name)
    if (_extension_registry.get(key) == code and
            _inverted_registry.get(code) == key):
        return  # Redundant registrations are benign
    if key in _extension_registry:
        raise ValueError("key %s is already registered with code %s" %
                         (key, _extension_registry[key]))
    if code in _inverted_registry:
        raise ValueError("code %s is already in use for key %s" %
                         (code, _inverted_registry[code]))
    _extension_registry[key] = code
    _inverted_registry[code] = key


def remove_extension(module, name, code):
    """Unregister an extension code.  For testing only."""
    key = (module, name)
    if (_extension_registry.get(key) != code or
            _inverted_registry.get(code) != key):
        raise ValueError("key %s is not registered with code %s" %
                         (key, code))
    del _extension_registry[key]
    del _inverted_registry[code]
    if code in _extension_cache:
        del _extension_cache[code]


def clear_extension_cache():
    _extension_cache.clear()


__all__ = ["pickle", "constructor", "dispatch_table",
           "add_extension", "remove_extension", "clear_extension_cache",
           "__newobj__", "__newobj_ex__", "_reconstructor",
           "_slotnames"]
