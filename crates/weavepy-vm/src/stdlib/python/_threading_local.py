"""Thread-local objects.

(Note that this module provides a Python version of the threading.local
 class.  Depending on the version of Python you're using, there may be a
 faster one available.  You should always import the `local` class from
 `threading`.)

WeavePy note (RFC 0039): CPython ships a C ``_thread._local``; ``threading``
re-exports it as ``local``. WeavePy has no native ``_thread._local`` yet, so
``threading`` falls back to this module.

CPython's upstream pure-Python implementation swaps the calling thread's dict
into the instance ``__dict__`` (``object.__setattr__(self, '__dict__', dct)``)
and then defers to ``object.__getattribute__``. That trick relies on
``obj.__dict__ = d`` *aliasing* ``d`` (so writes flow back into the per-thread
dict). WeavePy currently *copies* on ``__dict__`` assignment, so the swap can't
work. Instead we resolve attributes manually against the per-thread dict: a
lookup checks the current thread's slot *first* and only then falls back to
normal type lookup. This reproduces the observable behaviour of the C type —
in particular a per-thread attribute correctly *shadows* a class attribute
defined on a ``local`` subclass (e.g. ``asyncio.events._RunningLoop.loop_pid =
(None, None)``). A ``__getattr__``-only shim cannot do this, because normal
lookup succeeds on the class attribute and ``__getattr__`` never fires.

Two WeavePy-specific deviations from upstream:

* The thread-local dicts are keyed on ``_thread.get_ident()`` instead of
  ``id(threading.current_thread())``. Importing ``threading`` here would be
  circular, and the native worker teardown already hands us idents.
* Instead of a per-thread ``ref(thread, thread_deleted)`` weakref callback, a
  terminating thread's slots are dropped by ``_clear_thread(ident)``, invoked
  from the native worker teardown (see
  ``crates/weavepy-vm/src/stdlib/thread_real.rs``) the instant a thread dies.
  This is what lets a ``threading._DummyThread`` created by a *foreign* thread
  be finalised — and removed from ``threading._active`` — when that thread
  exits (``test_threading.test_foreign_thread``).
"""

import _thread
import weakref as _weakref

__all__ = ["local"]


# Registry of every live ``_localimpl`` so the native teardown can evict a
# dead thread's slots. CPython relies on a per-thread ``ref(thread,
# thread_deleted)``; WeavePy keys on idents and drives eviction via
# ``_clear_thread``.
_locals_lock = _thread.allocate_lock()
_all_impls = {}


def _register_impl(impl):
    key = id(impl)

    def _drop(_ref, key=key):
        with _locals_lock:
            _all_impls.pop(key, None)

    try:
        ref = _weakref.ref(impl, _drop)
    except TypeError:
        return
    with _locals_lock:
        _all_impls[key] = ref


def _clear_thread(thread_ident):
    """Drop *thread_ident*'s per-thread dict from every live ``local``.

    Invoked from the native worker teardown the moment a thread dies, so
    objects parked in thread-local storage (notably
    ``threading._DeleteDummyThreadOnDel``) are finalised promptly — the same
    instant CPython would clear the thread-state dict.

    Returns the number of non-empty per-thread dicts that were detached, which
    the native caller uses to decide whether a cycle collection is worth
    running to finalise what we just unlinked.
    """
    cleared = 0
    with _locals_lock:
        refs = list(_all_impls.values())
    for ref in refs:
        impl = ref()
        if impl is None:
            continue
        dct = impl.dicts.pop(thread_ident, None)
        if dct:
            cleared += 1
            # Drop each parked value's binding; the native caller runs a
            # collection afterwards so any ``__del__`` (e.g.
            # ``threading._DeleteDummyThreadOnDel.__del__``, which pops the
            # dummy from ``threading._active``) fires now rather than being
            # deferred to the next allocation-driven GC.
            dct.clear()
    return cleared


class _localimpl:
    """Owns the ``{ ident -> per-thread dict }`` mapping for one ``local``."""

    __slots__ = ('dicts', 'localargs', '__weakref__')

    def __init__(self, args, kw):
        self.dicts = {}
        self.localargs = (args, kw)


def _slot(self, impl):
    """Return the calling thread's dict for *self*.

    On the first touch from a thread the dict is created and — matching
    CPython — the subclass ``__init__`` runs once for that thread. The dict is
    inserted *before* ``__init__`` runs so the attribute writes ``__init__``
    performs re-enter through the (already-present) slot rather than recursing
    forever.
    """
    ident = _thread.get_ident()
    dicts = impl.dicts
    dct = dicts.get(ident)
    if dct is None:
        dct = {}
        dicts[ident] = dct
        args, kw = impl.localargs
        cls = type(self)
        if cls.__init__ is not object.__init__:
            cls.__init__(self, *args, **kw)
    return dct


class local:
    __slots__ = ('_local__impl',)

    def __new__(cls, /, *args, **kw):
        if (args or kw) and (cls.__init__ is object.__init__):
            raise TypeError("Initialization arguments are not supported")
        self = object.__new__(cls)
        impl = _localimpl(args, kw)
        object.__setattr__(self, '_local__impl', impl)
        _register_impl(impl)
        return self

    def __getattribute__(self, name):
        impl = object.__getattribute__(self, '_local__impl')
        slot = _slot(self, impl)
        # ``local().__dict__`` is the calling thread's namespace, mirroring the
        # C type (and ``test__threading_local``).
        if name == '__dict__':
            return slot
        # The per-thread slot acts as the instance dict: a value stored here
        # shadows a like-named class attribute, exactly like the C type.
        if name in slot:
            return slot[name]
        return object.__getattribute__(self, name)

    def __setattr__(self, name, value):
        if name == '__dict__':
            raise AttributeError(
                "%r object attribute '__dict__' is read-only"
                % type(self).__name__)
        impl = object.__getattribute__(self, '_local__impl')
        _slot(self, impl)[name] = value

    def __delattr__(self, name):
        if name == '__dict__':
            raise AttributeError(
                "%r object attribute '__dict__' is read-only"
                % type(self).__name__)
        impl = object.__getattribute__(self, '_local__impl')
        slot = _slot(self, impl)
        if name in slot:
            del slot[name]
        else:
            raise AttributeError(name)


# CPython ships ``local`` as the C type ``_thread._local`` and ``threading``
# re-exports it (``from _thread import _local as local``). Tests such as
# ``test_threading.MiscTestCase.test__all__`` therefore expect
# ``threading.local.__module__ == '_thread'``. We present this
# behaviourally-equivalent fallback under the same identity so that
# introspection (and the ``__all__`` audit) matches upstream.
local.__module__ = '_thread'
local.__qualname__ = '_local'
