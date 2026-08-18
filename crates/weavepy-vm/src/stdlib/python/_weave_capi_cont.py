"""RFC 0068 WS3 — dict/list/tuple/set C-API fixture shims.

Python ports of the `Modules/_testlimitedcapi/{dict,list,tuple,set}.c`
(and matching `Modules/_testcapi/*.c`) fixture wrappers, star-imported
by the frozen `_testcapi` / `_testlimitedcapi` shims. Every exported
name must be listed in `__all__`.

Conventions used throughout:
  * `None` arguments stand for C `NULL` (the C wrappers run them
    through the NULLABLE() macro).  Argument combinations marked
    "# CRASHES" in the test suite are not emulated.
  * `_NULL` is the stand-in for a NULL PyObject* slot inside a
    container (e.g. what `PyTuple_New`/`PyList_New` leave behind);
    its repr is "<NULL>" to match CPython's debug repr of such slots.
  * "bad internal call" SystemErrors mirror PyErr_BadInternalCall().
"""

__all__ = [
    # dict (_testlimitedcapi/dict.c + _testcapi/dict.c)
    "dict_check", "dict_checkexact", "dict_new", "dictproxy_new",
    "dict_clear", "dict_copy", "dict_size", "dict_getitem",
    "dict_getitemstring", "dict_getitemwitherror", "dict_getitemref",
    "dict_getitemstringref", "dict_contains", "dict_containsstring",
    "dict_setitem", "dict_setitemstring", "dict_delitem",
    "dict_delitemstring", "dict_setdefault", "dict_setdefaultref",
    "dict_keys", "dict_values", "dict_items", "dict_next",
    "dict_merge", "dict_update", "dict_mergefromseq2",
    "dict_pop", "dict_pop_null", "dict_popstring", "dict_popstring_null",
    # list (_testlimitedcapi/list.c + _testcapi/list.c)
    "list_check", "list_check_exact", "list_new", "list_size",
    "list_get_size", "list_getitem", "list_get_item_ref", "list_get_item",
    "list_setitem", "list_set_item", "list_insert", "list_append",
    "list_getslice", "list_setslice", "list_sort", "list_reverse",
    "list_astuple", "list_clear", "list_extend",
    # tuple (_testlimitedcapi/tuple.c + _testcapi/tuple.c)
    "tuple_check", "tuple_checkexact", "tuple_new", "tuple_pack",
    "tuple_size", "tuple_get_size", "tuple_getitem", "tuple_get_item",
    "tuple_getslice", "tuple_setitem", "tuple_set_item",
    "_tuple_resize", "_check_tuple_item_is_NULL",
    # set (_testlimitedcapi/set.c + _testcapi/set.c)
    "set_check", "set_checkexact", "frozenset_check",
    "frozenset_checkexact", "anyset_check", "anyset_checkexact",
    "set_new", "frozenset_new", "set_size", "set_get_size",
    "set_contains", "set_add", "set_discard", "set_pop", "set_clear",
]


class _NullSentinel:
    """Stand-in for a NULL PyObject* stored inside a container."""
    __slots__ = ()

    def __repr__(self):
        return "<NULL>"


_NULL = _NullSentinel()

# Allocation sizes above this raise MemoryError, mimicking what a real
# PyTuple_New / PyList_New of PY_SSIZE_T_MAX elements would do.
_MEM_LIMIT = 2**31


def _bad_internal_call():
    raise SystemError("bad argument to internal function")


def _decode_key(key):
    """Emulate the "z#" argument: accept str, or utf-8 decode bytes."""
    if isinstance(key, str):
        return key
    return bytes(key).decode("utf-8")


class _UnraisableHookArgs:
    __slots__ = ("exc_type", "exc_value", "exc_traceback", "err_msg",
                 "object")

    def __init__(self, exc, err_msg, obj):
        self.exc_type = type(exc)
        self.exc_value = exc
        self.exc_traceback = exc.__traceback__
        self.err_msg = err_msg
        self.object = obj


def _write_unraisable(exc, err_msg, obj=None):
    """Emulate PyErr_FormatUnraisable(): swallow `exc`, reporting it
    through sys.unraisablehook (which the tests replace and inspect)."""
    import sys
    hook = getattr(sys, "unraisablehook", None)
    if hook is None:
        return
    try:
        hook(_UnraisableHookArgs(exc, err_msg, obj))
    except Exception:
        pass


def _clamp_slice(n, ilow, ihigh):
    """PyList_GetSlice/SetSlice + PyTuple_GetSlice index clamping (no
    negative-index wrapping)."""
    if ilow < 0:
        ilow = 0
    elif ilow > n:
        ilow = n
    if ihigh < ilow:
        ihigh = ilow
    elif ihigh > n:
        ihigh = n
    return ilow, ihigh


# ---------------------------------------------------------------------------
# dict
# ---------------------------------------------------------------------------

def dict_check(obj):
    return int(isinstance(obj, dict))


def dict_checkexact(obj):
    return int(type(obj) is dict)


def dict_new():
    return {}


def dictproxy_new(mapping):
    from types import MappingProxyType
    # mappingproxy_check_mapping(): require a mapping (mp_subscript)
    # that is not a list or tuple.
    if (not hasattr(type(mapping), "__getitem__")
            or isinstance(mapping, (list, tuple))):
        raise TypeError("mappingproxy() argument must be a mapping, not %s"
                        % type(mapping).__name__)
    return MappingProxyType(mapping)


def dict_clear(mapping):
    # PyDict_Clear() is a no-op for non-dicts and returns void.
    if isinstance(mapping, dict):
        dict.clear(mapping)
    return None


def dict_copy(mapping):
    if mapping is None or not isinstance(mapping, dict):
        _bad_internal_call()
    return {key: value for key, value in dict.items(mapping)}


def dict_size(mapping):
    if not isinstance(mapping, dict):
        _bad_internal_call()
    return dict.__len__(mapping)


def dict_getitem(mapping, key):
    # PyDict_GetItem(): swallows all lookup errors (reported as an
    # unraisable exception); the wrapper maps a NULL result without a
    # pending error to the KeyError *class*.
    if not isinstance(mapping, dict):
        return KeyError
    try:
        value = dict.get(mapping, key, _NULL)
    except BaseException as exc:
        _write_unraisable(
            exc,
            "Exception ignored in PyDict_GetItem(); consider using "
            "PyDict_GetItemRef()")
        return KeyError
    if value is _NULL:
        return KeyError
    return value


def dict_getitemstring(mapping, key):
    try:
        skey = _decode_key(key)
    except BaseException as exc:
        _write_unraisable(
            exc,
            "Exception ignored in PyDict_GetItemString(); consider using "
            "PyDict_GetItemStringRef()")
        return KeyError
    return dict_getitem(mapping, skey)


def dict_getitemwitherror(mapping, key):
    # PyDict_GetItemWithError(): propagates errors.
    if not isinstance(mapping, dict):
        _bad_internal_call()
    value = dict.get(mapping, key, _NULL)
    if value is _NULL:
        return KeyError
    return value


def dict_getitemref(mapping, key):
    # PyDict_GetItemRef(): same observable behavior as WithError here.
    return dict_getitemwitherror(mapping, key)


def dict_getitemstringref(mapping, key):
    return dict_getitemwitherror(mapping, _decode_key(key))


def dict_contains(mapping, key):
    return int(dict.__contains__(mapping, key))


def dict_containsstring(mapping, key):
    return int(dict.__contains__(mapping, _decode_key(key)))


def dict_setitem(mapping, key, value):
    if not isinstance(mapping, dict):
        _bad_internal_call()
    dict.__setitem__(mapping, key, value)
    return 0


def dict_setitemstring(mapping, key, value):
    return dict_setitem(mapping, _decode_key(key), value)


def dict_delitem(mapping, key):
    if not isinstance(mapping, dict):
        _bad_internal_call()
    dict.__delitem__(mapping, key)
    return 0


def dict_delitemstring(mapping, key):
    return dict_delitem(mapping, _decode_key(key))


def dict_setdefault(mapping, key, defaultobj):
    if not isinstance(mapping, dict):
        _bad_internal_call()
    return dict.setdefault(mapping, key, defaultobj)


def dict_setdefaultref(mapping, key, default_value):
    return dict_setdefault(mapping, key, default_value)


def dict_keys(mapping):
    if not isinstance(mapping, dict):
        _bad_internal_call()
    return list(dict.keys(mapping))


def dict_values(mapping):
    if not isinstance(mapping, dict):
        _bad_internal_call()
    return list(dict.values(mapping))


def dict_items(mapping):
    if not isinstance(mapping, dict):
        _bad_internal_call()
    return list(dict.items(mapping))


def dict_next(mapping, pos):
    # PyDict_Next(): entry order for a fresh dict matches insertion
    # order, and pos advances past the consumed slot.
    if not isinstance(mapping, dict):
        return None
    entries = list(dict.items(mapping))
    if pos < 0 or pos >= len(entries):
        return None
    key, value = entries[pos]
    return (1, pos + 1, key, value)


def dict_merge(mapping, mapping2, override):
    # PyDict_Merge()
    if mapping is None or not isinstance(mapping, dict) or mapping2 is None:
        _bad_internal_call()
    if isinstance(mapping2, dict):
        # Fast path: read entries directly, bypassing subclass hooks.
        for key, value in list(dict.items(mapping2)):
            if override or not dict.__contains__(mapping, key):
                dict.__setitem__(mapping, key, value)
    else:
        # Generic path: b.keys() (AttributeError if missing) + b[key].
        keys = mapping2.keys()
        for key in list(keys):
            if override != 1 and dict.__contains__(mapping, key):
                continue
            dict.__setitem__(mapping, key, mapping2[key])
    return 0


def dict_update(mapping, mapping2):
    return dict_merge(mapping, mapping2, 1)


def dict_mergefromseq2(mapping, seq2, override):
    # PyDict_MergeFromSeq2()
    if mapping is None or seq2 is None:
        _bad_internal_call()
    for i, item in enumerate(iter(seq2)):
        if isinstance(item, (list, tuple)):
            fast = item
        else:
            try:
                fast = list(item)
            except TypeError:
                raise TypeError(
                    "cannot convert dictionary update sequence element #%d "
                    "to a sequence" % i) from None
        n = len(fast)
        if n != 2:
            raise ValueError(
                "dictionary update sequence element #%d has length %d; "
                "2 is required" % (i, n))
        key = fast[0]
        value = fast[1]
        if override or not dict.__contains__(mapping, key):
            dict.__setitem__(mapping, key, value)
    return 0


def _dict_pop_impl(dct, key):
    # PyDict_Pop(): dict check first, then an empty-dict fast path that
    # never hashes the key, then a normal (hashing) pop.
    if not isinstance(dct, dict):
        _bad_internal_call()
    if dict.__len__(dct) == 0:
        return (0, None)
    value = dict.pop(dct, key, _NULL)
    if value is _NULL:
        return (0, None)
    return (1, value)


def dict_pop(dct, key):
    return _dict_pop_impl(dct, key)


def dict_pop_null(dct, key):
    return _dict_pop_impl(dct, key)[0]


def dict_popstring(dct, key):
    return _dict_pop_impl(dct, _decode_key(key))


def dict_popstring_null(dct, key):
    return _dict_pop_impl(dct, _decode_key(key))[0]


# ---------------------------------------------------------------------------
# list
# ---------------------------------------------------------------------------

def list_check(obj):
    return int(isinstance(obj, list))


def list_check_exact(obj):
    return int(type(obj) is list)


def list_new(size):
    # PyList_New(PyLong_AsSsize_t(obj)): a bad size (None, negative)
    # ends up as PyErr_BadInternalCall via PyList_New(-1).
    if size is None or not isinstance(size, int) or size < 0:
        _bad_internal_call()
    if size > _MEM_LIMIT:
        raise MemoryError
    return [_NULL] * size


def list_size(obj):
    if not isinstance(obj, list):
        _bad_internal_call()
    return list.__len__(obj)


def list_get_size(obj):
    # PyList_GET_SIZE(): no checks; only valid lists are tested.
    return list.__len__(obj)


def list_getitem(obj, i):
    # PyList_GetItem(): no negative-index wrapping.
    if not isinstance(obj, list):
        _bad_internal_call()
    if not 0 <= i < list.__len__(obj):
        raise IndexError("list index out of range")
    return list.__getitem__(obj, i)


def list_get_item_ref(obj, i):
    # PyList_GetItemRef(): TypeError (not SystemError) for non-lists.
    if not isinstance(obj, list):
        raise TypeError("expected a list")
    if not 0 <= i < list.__len__(obj):
        raise IndexError("list index out of range")
    return list.__getitem__(obj, i)


def list_get_item(obj, i):
    # PyList_GET_ITEM(): no checks.
    return list.__getitem__(obj, i)


def list_setitem(obj, i, value):
    if not isinstance(obj, list):
        _bad_internal_call()
    if not 0 <= i < list.__len__(obj):
        raise IndexError("list assignment index out of range")
    list.__setitem__(obj, i, _NULL if value is None else value)
    return 0


def list_set_item(obj, i, value):
    # PyList_SET_ITEM(): no checks; the wrapper returns None.
    list.__setitem__(obj, i, _NULL if value is None else value)
    return None


def list_insert(obj, where, value):
    if not isinstance(obj, list):
        _bad_internal_call()
    list.insert(obj, where, value)
    return 0


def list_append(obj, value):
    # PyList_Append(NULL item) is a bad internal call too.
    if value is None or not isinstance(obj, list):
        _bad_internal_call()
    list.append(obj, value)
    return 0


def list_getslice(obj, ilow, ihigh):
    if not isinstance(obj, list):
        _bad_internal_call()
    ilow, ihigh = _clamp_slice(list.__len__(obj), ilow, ihigh)
    return list.__getitem__(obj, slice(ilow, ihigh))


def list_setslice(obj, ilow, ihigh, value):
    # PyList_SetSlice(); value == NULL deletes the slice.
    if not isinstance(obj, list):
        _bad_internal_call()
    ilow, ihigh = _clamp_slice(list.__len__(obj), ilow, ihigh)
    if value is None:
        list.__delitem__(obj, slice(ilow, ihigh))
    else:
        if not isinstance(value, (list, tuple)):
            value = list(value)
        list.__setitem__(obj, slice(ilow, ihigh), value)
    return 0


def list_sort(obj):
    if not isinstance(obj, list):
        _bad_internal_call()
    if type(obj) is list:
        list.sort(obj)
    else:
        # WeavePy's unbound list.sort rejects subclass receivers;
        # emulate the in-place sort via slice assignment instead.
        list.__setitem__(obj, slice(None),
                         sorted(list.__getitem__(obj, slice(None))))
    return 0


def list_reverse(obj):
    if not isinstance(obj, list):
        _bad_internal_call()
    list.reverse(obj)
    return 0


def list_astuple(obj):
    if not isinstance(obj, list):
        _bad_internal_call()
    return tuple(obj)


def list_clear(obj):
    if not isinstance(obj, list):
        _bad_internal_call()
    list.clear(obj)
    return 0


def list_extend(obj, arg):
    if not isinstance(obj, list):
        _bad_internal_call()
    list.extend(obj, arg)
    return 0


# ---------------------------------------------------------------------------
# tuple
# ---------------------------------------------------------------------------

def tuple_check(obj):
    return int(isinstance(obj, tuple))


def tuple_checkexact(obj):
    return int(type(obj) is tuple)


def tuple_new(size):
    # PyTuple_New(): slots start out NULL.
    if size is None or not isinstance(size, int) or size < 0:
        _bad_internal_call()
    if size > _MEM_LIMIT:
        raise MemoryError
    return (_NULL,) * size


def tuple_pack(size, *args):
    # PyTuple_Pack(size, ...): size drives PyTuple_New.
    if size is None or not isinstance(size, int) or size < 0:
        _bad_internal_call()
    if size > _MEM_LIMIT:
        raise MemoryError
    packed = tuple(_NULL if arg is None else arg for arg in args)
    return packed[:size]


def tuple_size(obj):
    if not isinstance(obj, tuple):
        _bad_internal_call()
    return tuple.__len__(obj)


def tuple_get_size(obj):
    # PyTuple_GET_SIZE(): no checks.
    return tuple.__len__(obj)


def tuple_getitem(obj, i):
    # PyTuple_GetItem(): no negative-index wrapping.
    if not isinstance(obj, tuple):
        _bad_internal_call()
    if not 0 <= i < tuple.__len__(obj):
        raise IndexError("tuple index out of range")
    return tuple.__getitem__(obj, i)


def tuple_get_item(obj, i):
    # PyTuple_GET_ITEM(): no checks.
    return tuple.__getitem__(obj, i)


def tuple_getslice(obj, ilow, ihigh):
    if not isinstance(obj, tuple):
        _bad_internal_call()
    ilow, ihigh = _clamp_slice(tuple.__len__(obj), ilow, ihigh)
    result = tuple.__getitem__(obj, slice(ilow, ihigh))
    if type(result) is not tuple:
        result = tuple(result)
    return result


def tuple_setitem(obj, i, value):
    # The wrapper copies exact tuples and calls PyTuple_SetItem on the
    # copy; anything else (subclass instances included, whose refcount
    # is never 1 here) is a bad internal call.
    if type(obj) is tuple:
        if not 0 <= i < len(obj):
            raise IndexError("tuple assignment index out of range")
        item = _NULL if value is None else value
        return obj[:i] + (item,) + obj[i + 1:]
    _bad_internal_call()


def tuple_set_item(obj, i, value):
    # PyTuple_SET_ITEM() wrapper: exact tuples get a mutated copy
    # (the C fixture builds a fresh tuple for the exact case too). A
    # tuple *subclass* is mutated in place — CPython writes into the
    # object struct's item array — via the native helper, preserving
    # instance identity (the test asserts `assertIs`).
    if type(obj) is tuple:
        item = _NULL if value is None else value
        return obj[:i] + (item,) + obj[i + 1:]
    if isinstance(obj, tuple):
        import _testinternalcapi
        item = _NULL if value is None else value
        return _testinternalcapi._tuple_subclass_set_item(obj, i, item)
    raise SystemError("bad argument to internal function")


def _tuple_resize(tup, newsize, new=True):
    # _PyTuple_Resize() via the _testcapi wrapper.  With new=True the
    # wrapper operates on a fresh copy (refcount 1); with new=False any
    # non-empty tuple coming from Python code has refcount > 1 and is
    # rejected by _PyTuple_Resize as a bad internal call.
    if new:
        if tup is None or not isinstance(tup, tuple):
            _bad_internal_call()
        base = tuple(tup) if type(tup) is not tuple else tup
    else:
        if tup is None or type(tup) is not tuple:
            _bad_internal_call()
        if tuple.__len__(tup) != 0:
            _bad_internal_call()
        base = tup
    if newsize is None or not isinstance(newsize, int):
        _bad_internal_call()
    old = len(base)
    if old == newsize:
        return base
    if newsize < 0:
        _bad_internal_call()
    if newsize > _MEM_LIMIT:
        raise MemoryError
    if newsize < old:
        return base[:newsize]
    return base + (_NULL,) * (newsize - old)


def _check_tuple_item_is_NULL(obj, i):
    return int(tuple.__getitem__(obj, i) is _NULL)


# ---------------------------------------------------------------------------
# set
# ---------------------------------------------------------------------------

def set_check(obj):
    return int(isinstance(obj, set))


def set_checkexact(obj):
    return int(type(obj) is set)


def frozenset_check(obj):
    return int(isinstance(obj, frozenset))


def frozenset_checkexact(obj):
    return int(type(obj) is frozenset)


def anyset_check(obj):
    return int(isinstance(obj, (set, frozenset)))


def anyset_checkexact(obj):
    return int(type(obj) is set or type(obj) is frozenset)


def set_new(*args):
    # PySet_New(NULL) -> empty set.
    if not args:
        return set()
    return set(args[0])


def frozenset_new(*args):
    if not args:
        return frozenset()
    return frozenset(args[0])


def set_size(obj):
    # PySet_Size(): any set or frozenset (incl. subclasses).
    if isinstance(obj, set):
        return set.__len__(obj)
    if isinstance(obj, frozenset):
        return frozenset.__len__(obj)
    _bad_internal_call()


def set_get_size(obj):
    # PySet_GET_SIZE(): no checks; only valid inputs are tested.
    if isinstance(obj, set):
        return set.__len__(obj)
    return frozenset.__len__(obj)


def set_contains(obj, item):
    if isinstance(obj, set):
        return int(set.__contains__(obj, item))
    if isinstance(obj, frozenset):
        return int(frozenset.__contains__(obj, item))
    _bad_internal_call()


def set_add(obj, item):
    # PySet_Add(): frozensets reaching here always have refcount > 1,
    # so they are rejected just like arbitrary objects.
    if not isinstance(obj, set):
        _bad_internal_call()
    set.add(obj, item)
    return 0


def set_discard(obj, item):
    # PySet_Discard(): 1 if found and removed, 0 otherwise.
    if not isinstance(obj, set):
        _bad_internal_call()
    found = set.__contains__(obj, item)
    if found:
        set.discard(obj, item)
    return int(found)


def set_pop(obj):
    if not isinstance(obj, set):
        _bad_internal_call()
    return set.pop(obj)


def set_clear(obj):
    if not isinstance(obj, set):
        _bad_internal_call()
    set.clear(obj)
    return 0
