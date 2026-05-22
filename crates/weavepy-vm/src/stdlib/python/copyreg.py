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
                    if name.startswith("__") and not name.endswith("__"):
                        names.append("_" + c.__name__ + name)
                    else:
                        names.append(name)
        slotnames = names
    try:
        cls.__slotnames__ = slotnames
    except TypeError:
        pass
    return slotnames


__all__ = ["pickle", "constructor", "dispatch_table",
           "__newobj__", "__newobj_ex__", "_reconstructor",
           "_slotnames"]
