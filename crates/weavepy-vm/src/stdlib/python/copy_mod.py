"""Shallow / deep copy operations — WeavePy port of CPython's ``copy``.

Mirrors the public surface: :func:`copy.copy` and
:func:`copy.deepcopy`, with the ``__copy__`` / ``__deepcopy__``
protocol honoured. The dispatch tables for the immutable atomic
types match CPython.
"""


class Error(Exception):
    pass


error = Error

# Sentinel for the deepcopy memo lookup miss.
_nil = object()


def copy(x):
    cls = type(x)

    copier = _copy_dispatch.get(cls)
    if copier:
        return copier(x)

    copier = getattr(x, "__copy__", None)
    if copier is not None:
        return copier()

    reductor = getattr(x, "__reduce_ex__", None)
    if reductor is not None:
        rv = reductor(4)
    else:
        reductor = getattr(x, "__reduce__", None)
        if reductor:
            rv = reductor()
        else:
            raise Error("un(shallow)copyable object of type %s" % cls)
    if isinstance(rv, str):
        return x
    return _reconstruct(x, None, *rv)


_copy_dispatch = d = {}


def _copy_immutable(x):
    return x


for t in (
    type(None),
    int,
    float,
    bool,
    complex,
    str,
    tuple,
    bytes,
    frozenset,
    type,
    range,
    slice,
    type(Ellipsis),
    type(NotImplemented),
):
    d[t] = _copy_immutable


def _copy_list(x):
    return x.copy()


d[list] = _copy_list


def _copy_dict(x):
    return x.copy()


d[dict] = _copy_dict


def _copy_set(x):
    return x.copy()


d[set] = _copy_set


def _copy_bytearray(x):
    return x[:]


d[bytearray] = _copy_bytearray


def deepcopy(x, memo=None, _nil=[]):
    if memo is None:
        memo = {}
    d = id(x)
    y = memo.get(d, _nil)
    if y is not _nil:
        return y
    cls = type(x)
    copier = _deepcopy_dispatch.get(cls)
    if copier is not None:
        y = copier(x, memo)
    else:
        if cls is type:
            y = x
        else:
            copier = getattr(x, "__deepcopy__", None)
            if copier is not None:
                y = copier(memo)
            else:
                reductor = getattr(x, "__reduce_ex__", None)
                if reductor:
                    rv = reductor(4)
                else:
                    reductor = getattr(x, "__reduce__", None)
                    if reductor:
                        rv = reductor()
                    else:
                        raise Error("un(deep)copyable object of type %s" % cls)
                if isinstance(rv, str):
                    y = x
                else:
                    y = _reconstruct(x, memo, *rv)

    if y is not x:
        memo[d] = y
        _keep_alive(x, memo)
    return y


_deepcopy_dispatch = dd = {}


def _deepcopy_atomic(x, memo):
    return x


for t in (
    type(None),
    int,
    float,
    bool,
    complex,
    str,
    bytes,
    type,
    range,
    type(Ellipsis),
    type(NotImplemented),
):
    dd[t] = _deepcopy_atomic


def _deepcopy_list(x, memo, deepcopy=deepcopy):
    y = []
    memo[id(x)] = y
    append = y.append
    for a in x:
        append(deepcopy(a, memo))
    return y


dd[list] = _deepcopy_list


def _deepcopy_tuple(x, memo, deepcopy=deepcopy):
    y = [deepcopy(a, memo) for a in x]
    try:
        return memo[id(x)]
    except KeyError:
        pass
    for k, j in zip(x, y):
        if k is not j:
            y = tuple(y)
            break
    else:
        y = x
    memo[id(x)] = y
    return y


dd[tuple] = _deepcopy_tuple


def _deepcopy_dict(x, memo, deepcopy=deepcopy):
    y = {}
    memo[id(x)] = y
    for key, value in x.items():
        y[deepcopy(key, memo)] = deepcopy(value, memo)
    return y


dd[dict] = _deepcopy_dict


def _deepcopy_set(x, memo, deepcopy=deepcopy):
    y = set()
    memo[id(x)] = y
    for a in x:
        y.add(deepcopy(a, memo))
    return y


dd[set] = _deepcopy_set


def _deepcopy_frozenset(x, memo, deepcopy=deepcopy):
    return frozenset(deepcopy(a, memo) for a in x)


dd[frozenset] = _deepcopy_frozenset


def _deepcopy_bytearray(x, memo):
    y = x[:]
    memo[id(x)] = y
    return y


dd[bytearray] = _deepcopy_bytearray


def _keep_alive(x, memo):
    try:
        memo[id(memo)].append(x)
    except KeyError:
        memo[id(memo)] = [x]


def _reconstruct(x, memo, func, args, state=None, listiter=None, dictiter=None, deepcopy=deepcopy):
    deep = memo is not None
    if deep and args:
        args = (deepcopy(arg, memo) for arg in args)
    y = func(*args)
    if deep:
        memo[id(x)] = y
    if state is not None:
        if deep:
            state = deepcopy(state, memo)
        if hasattr(y, "__setstate__"):
            y.__setstate__(state)
        else:
            if isinstance(state, tuple) and len(state) == 2:
                state, slotstate = state
            else:
                slotstate = None
            if state is not None:
                d = y.__dict__
                for key, value in state.items():
                    d[key] = value
            if slotstate is not None:
                for key, value in slotstate.items():
                    setattr(y, key, value)
    if listiter is not None:
        if deep:
            for item in listiter:
                item = deepcopy(item, memo)
                y.append(item)
        else:
            for item in listiter:
                y.append(item)
    if dictiter is not None:
        if deep:
            for key, value in dictiter:
                key = deepcopy(key, memo)
                value = deepcopy(value, memo)
                y[key] = value
        else:
            for key, value in dictiter:
                y[key] = value
    return y


__all__ = ["Error", "copy", "deepcopy"]
