"""CPython 3.13's `_interpqueues` — cross-interpreter queues.

CPython implements this as the C extension
`Modules/_interpqueuesmodule.c` over a process-global queue registry.
WeavePy's registry lives in the native `_xxsubinterpreters` module
(`interpreters_mod.rs`) — addressable from any interpreter — so this
frozen shim only adapts calling conventions and retypes backend
errors into the `_interpqueues` exception hierarchy.

`test.support.interpreters.queues` is the sole stdlib consumer.
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


def create(maxsize, fmt, unboundop):
    try:
        return _backend.queue_create(maxsize, fmt, unboundop)
    except (RuntimeError, ValueError) as exc:
        raise _map_error(exc) from None


def destroy(qid):
    try:
        _backend.queue_destroy(int(qid))
    except (RuntimeError, ValueError) as exc:
        raise _map_error(exc) from None


def list_all():
    try:
        return [tuple(entry) for entry in _backend.queue_list_all()]
    except (RuntimeError, ValueError) as exc:
        raise _map_error(exc) from None


def get_queue_defaults(qid):
    try:
        return tuple(_backend.queue_get_defaults(int(qid)))
    except (RuntimeError, ValueError) as exc:
        raise _map_error(exc) from None


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


def put(qid, obj, fmt, unboundop):
    try:
        _backend.queue_put(int(qid), obj, fmt, unboundop)
    except TypeError as exc:
        if 'not shareable' in str(exc):
            # `_interpreters.NotShareableError` derives from ValueError
            # (test_interpreters test_queues asserts
            # `interpreters.NotShareableError` from `queue.put`).
            import _interpreters
            raise _interpreters.NotShareableError(str(exc)) from None
        raise
    except (RuntimeError, ValueError) as exc:
        if 'not shareable' in str(exc):
            raise
        raise _map_error(exc) from None


def get(qid):
    try:
        return tuple(_backend.queue_get(int(qid)))
    except (RuntimeError, ValueError) as exc:
        raise _map_error(exc) from None
