"""Target-side half of ``_interpreters.call()`` (PEP 734, CPython 3.14).

``_interpreters.call(id, callable, args, kwargs)`` converts the call to
cross-interpreter data (`_weave_xidata`: shareable as-is, stateless
functions by marshal, the rest by pickle) in the requesting interpreter,
parks the payload on a private cross-interpreter queue, and runs
``__import__('_weave_xicall').run(qid)`` in the target. That single expression statement binds nothing in the
target's ``__main__`` (test_interpreters checks ``interp.call(dir)``
reports only the pristine module names), and this module does the rest:
rebuild, call, and ship the converted result back on the same queue.

The result queue item is ``(kind, payload)``:

``('ok', payload)``    the return value as cross-interpreter data
``('unshareable', s)`` the callable/args or the return value could not
                       be converted; ``s`` is the error text and the
                       caller raises ``NotShareableError``

A callable that raises simply propagates out of ``run`` -- the caller's
``run_string`` sees the exception and packages it as ``excinfo``, exactly
like ``Interpreter.exec``.
"""

import builtins
import sys

import _xxsubinterpreters as _xx


def _main_namespace():
    main = sys.modules.get('__main__')
    return main.__dict__ if main is not None else {}


def _call_as_running_main(fn, args, kwargs):
    """Invoke ``fn`` the way CPython's ``_make_call`` does.

    CPython calls the object from C with no Python frame on the stack, so
    frame-sensitive builtins fall back to ``__main__`` via
    ``_PyEval_GetGlobalsFromRunningMain`` (3.14). Here the call is made
    from this helper's frame, so emulate that fallback for the builtins
    that would otherwise see *our* locals.
    """
    if not kwargs:
        if fn is builtins.exec or fn is builtins.eval:
            if len(args) == 1:
                ns = _main_namespace()
                return fn(args[0], ns, ns)
        elif fn is builtins.dir and not args:
            return sorted(_main_namespace())
        elif fn is builtins.globals and not args:
            return _main_namespace()
        elif fn is builtins.locals and not args:
            return _main_namespace()
        elif fn is builtins.vars and not args:
            return _main_namespace()
    return fn(*args, **kwargs)


def run(qid):
    import _weave_xidata as _xid
    (fn, args, kwargs), _fmt, _unbound = _xx.queue_get(qid)
    try:
        fn = _xid.from_xidata(fn)
        args = _xid.from_xidata(args) if args is not None else ()
        kwargs = _xid.from_xidata(kwargs) if kwargs is not None else {}
    except BaseException as exc:  # noqa: BLE001 - any conversion failure
        _xx.queue_put(qid, ('unshareable', f'{type(exc).__name__}: {exc}'),
                      0, 0)
        return
    res = _call_as_running_main(fn, args, kwargs)
    try:
        payload = _xid.get_xidata(res)
    except BaseException as exc:  # noqa: BLE001
        _xx.queue_put(qid, ('unshareable', f'{type(exc).__name__}: {exc}'),
                      0, 0)
        return
    _xx.queue_put(qid, ('ok', payload), 0, 0)
