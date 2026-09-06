"""CPython 3.14's `_interpqueues` — cross-interpreter queues.

CPython implements this as the C extension
`Modules/_interpqueuesmodule.c` over a process-global queue registry.
WeavePy's registry lives in the native `_xxsubinterpreters` module
(`interpreters_mod.rs`) — addressable from any interpreter — so this
frozen shim only adapts calling conventions and retypes backend
errors into the `_interpqueues` exception hierarchy.

`concurrent.interpreters._queues` (PEP 734) is the sole stdlib consumer.

3.14 replaced the 3.13 `fmt` slot (`_SHARED_ONLY` / `_PICKLED`, chosen
by the *wrapper*) with a per-queue / per-put `fallback`
(`_PyXIDATA_XIDATA_ONLY` = 0 / `_PyXIDATA_FULL_FALLBACK` = 1) resolved
*here*: items cross as cross-interpreter data (`_weave_xidata`), and
`get()` returns `(obj, unboundop)`. The backend's `fmt` slot now only
stores a queue's default fallback.
"""

import _xxsubinterpreters as _backend

__all__ = [
    'QueueError', 'QueueNotFoundError',
    'create', 'destroy', 'list_all', 'get_queue_defaults',
    'bind', 'release', 'get_maxsize', 'is_full', 'get_count',
    'put', 'get', '_register_heap_types',
]


class QueueError(RuntimeError):
    pass


class QueueNotFoundError(QueueError):
    pass


# The high-level wrapper (`test.support.interpreters.queues`) defines
# QueueEmpty/QueueFull as subclasses of *its* QueueError plus
# queue.Empty/queue.Full, and registers them here so the backend
# raises the exact classes callers catch (CPython's
# `_register_heap_types` does the same).
_queue_cls = None
_empty_cls = None
_full_cls = None


def _register_heap_types(queue_cls, empty_cls, full_cls):
    global _queue_cls, _empty_cls, _full_cls
    _queue_cls = queue_cls
    _empty_cls = empty_cls
    _full_cls = full_cls
    # CPython registers Queue in the XID registry: queue objects are
    # shareable (they reconstruct from their qid on the far side; in
    # our shared-heap model the instance itself crosses —
    # test_interpreters test_queues QueueTests.test_shareable).
    queue_cls._weave_xid_shareable = True


def _map_error(exc):
    """Retype a backend error into the `_interpqueues` hierarchy."""
    text = str(exc)
    if 'does not exist' in text:
        return QueueNotFoundError(text)
    if 'queue is empty' in text:
        cls = _empty_cls if _empty_cls is not None else QueueError
        return cls(text)
    if 'queue is full' in text:
        cls = _full_cls if _full_cls is not None else QueueError
        return cls(text)
    return QueueError(text)


# Modules/_interpreters_common.h
_UNBOUND_REMOVE = 1
_UNBOUND_ERROR = 2
_UNBOUND_REPLACE = 3
_XIDATA_ONLY = 0
_FULL_FALLBACK = 1


def _resolve_unboundop(arg, default):
    if arg < 0:
        return default
    if arg in (_UNBOUND_REMOVE, _UNBOUND_ERROR, _UNBOUND_REPLACE):
        return arg
    raise ValueError(f'unsupported unboundop {arg}')


def _resolve_fallback(arg, default):
    if arg < 0:
        return default
    if arg in (_XIDATA_ONLY, _FULL_FALLBACK):
        return arg
    raise ValueError(f'unsupported fallback {arg}')


def create(maxsize, unboundop=-1, fallback=-1):
    unboundop = _resolve_unboundop(unboundop, _UNBOUND_REPLACE)
    fallback = _resolve_fallback(fallback, _FULL_FALLBACK)
    try:
        return _backend.queue_create(maxsize, fallback, unboundop)
    except (RuntimeError, ValueError) as exc:
        raise _map_error(exc) from None


def destroy(qid):
    try:
        _backend.queue_destroy(int(qid))
    except (RuntimeError, ValueError) as exc:
        raise _map_error(exc) from None


def list_all():
    try:
        entries = _backend.queue_list_all()
    except (RuntimeError, ValueError) as exc:
        raise _map_error(exc) from None
    return [(qid, unboundop, fallback) for qid, fallback, unboundop in entries]


def get_queue_defaults(qid):
    try:
        fallback, unboundop = _backend.queue_get_defaults(int(qid))
    except (RuntimeError, ValueError) as exc:
        raise _map_error(exc) from None
    return (unboundop, fallback)


def bind(qid):
    try:
        _backend.queue_bind(int(qid))
    except (RuntimeError, ValueError) as exc:
        raise _map_error(exc) from None


def release(qid):
    try:
        _backend.queue_release(int(qid))
    except (RuntimeError, ValueError) as exc:
        raise _map_error(exc) from None


def get_maxsize(qid):
    try:
        return _backend.queue_get_maxsize(int(qid))
    except (RuntimeError, ValueError) as exc:
        raise _map_error(exc) from None


def is_full(qid):
    try:
        return _backend.queue_is_full(int(qid))
    except (RuntimeError, ValueError) as exc:
        raise _map_error(exc) from None


def get_count(qid):
    try:
        return _backend.queue_get_count(int(qid))
    except (RuntimeError, ValueError) as exc:
        raise _map_error(exc) from None


def put(qid, obj, unboundop=-1, fallback=-1):
    qid = int(qid)
    if unboundop < 0 or fallback < 0:
        default_unboundop, default_fallback = get_queue_defaults(qid)
    else:
        default_unboundop = default_fallback = None
    unboundop = _resolve_unboundop(unboundop, default_unboundop)
    fallback = _resolve_fallback(fallback, default_fallback)
    import _weave_xidata
    payload = _weave_xidata.get_xidata(obj, fallback)
    try:
        _backend.queue_put(qid, payload, 0, unboundop)
    except (RuntimeError, ValueError) as exc:
        raise _map_error(exc) from None


def get(qid):
    try:
        obj, _fmt, unboundop = _backend.queue_get(int(qid))
    except (RuntimeError, ValueError) as exc:
        raise _map_error(exc) from None
    if unboundop is not None:
        return (None, unboundop)
    import _weave_xidata
    return (_weave_xidata.from_xidata(obj), None)
