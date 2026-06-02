"""WeavePy's pure-Python ``contextlib`` module.

Provides the most common context-manager helpers: ``contextmanager``,
``closing``, ``suppress``, ``redirect_stdout``, ``redirect_stderr``,
``nullcontext``, and ``ExitStack``.
"""

import sys
from functools import wraps


__all__ = [
    "contextmanager",
    "ContextDecorator",
    "closing",
    "suppress",
    "redirect_stdout",
    "redirect_stderr",
    "nullcontext",
    "ExitStack",
]


class ContextDecorator:
    """A base class or mixin that enables context managers to work as
    decorators (PEP 343 / CPython ``contextlib.ContextDecorator``)."""

    def _recreate_cm(self):
        """Return a recreated instance of self.

        Allows an otherwise one-shot context manager like
        ``_GeneratorContextManager`` to be used as a decorator over
        multiple invocations of the wrapped function.
        """
        return self

    def __call__(self, func):
        @wraps(func)
        def inner(*args, **kwds):
            with self._recreate_cm():
                return func(*args, **kwds)
        return inner


class _GeneratorContextManager(ContextDecorator):
    """Wrap a generator function turned into a context manager.

    Inherits :class:`ContextDecorator` so a ``@contextmanager`` result
    can itself be used as a decorator (``@cm`` over a function), with
    ``_recreate_cm`` rebuilding the one-shot generator each call.
    """

    def __init__(self, func, args, kwds):
        self.gen = func(*args, **kwds)
        self.func, self.args, self.kwds = func, args, kwds
        # Carry the wrapped function's docstring, matching CPython.
        doc = getattr(func, "__doc__", None)
        if doc is None:
            doc = type(self).__doc__
        self.__doc__ = doc

    def _recreate_cm(self):
        return self.__class__(self.func, self.args, self.kwds)

    def __enter__(self):
        try:
            return next(self.gen)
        except StopIteration:
            raise RuntimeError("generator didn't yield") from None

    def __exit__(self, exc_type, exc_value, traceback):
        if exc_type is None:
            try:
                next(self.gen)
            except StopIteration:
                return False
            raise RuntimeError("generator didn't stop")
        try:
            self.gen.throw(exc_type, exc_value, traceback)
        except StopIteration as stop:
            return stop is not exc_value
        except BaseException as exc:
            if exc is exc_value:
                return False
            raise
        raise RuntimeError("generator didn't stop after throw()")


def contextmanager(func):
    """Decorator that turns a generator into a context-manager factory.

    The returned ``helper`` builds a fresh :class:`_GeneratorContextManager`
    per call and is ``functools.wraps``-decorated so the factory keeps the
    wrapped function's name/qualname/doc.
    """
    @wraps(func)
    def helper(*args, **kwargs):
        return _GeneratorContextManager(func, args, kwargs)
    return helper


class closing:
    """Context manager that calls ``close`` on its target."""

    def __init__(self, thing):
        self.thing = thing

    def __enter__(self):
        return self.thing

    def __exit__(self, *exc):
        self.thing.close()
        return False


class suppress:
    """Suppress one or more exception types."""

    def __init__(self, *exceptions):
        self._exceptions = exceptions

    def __enter__(self):
        return None

    def __exit__(self, exc_type, exc_value, traceback):
        if exc_type is None:
            return False
        for exc in self._exceptions:
            if issubclass(exc_type, exc):
                return True
        return False


class _RedirectStream:
    _stream = None

    def __init__(self, new_target):
        self._new_target = new_target
        self._old_targets = []

    def __enter__(self):
        self._old_targets.append(getattr(sys, self._stream))
        setattr(sys, self._stream, self._new_target)
        return self._new_target

    def __exit__(self, *exc):
        setattr(sys, self._stream, self._old_targets.pop())
        return False


class redirect_stdout(_RedirectStream):
    _stream = "stdout"


class redirect_stderr(_RedirectStream):
    _stream = "stderr"


class nullcontext:
    """Context manager that does nothing."""

    def __init__(self, enter_result=None):
        self.enter_result = enter_result

    def __enter__(self):
        return self.enter_result

    def __exit__(self, *exc):
        return False


class ExitStack:
    """Track and unwind multiple context managers."""

    def __init__(self):
        self._exit_callbacks = []

    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc_value, traceback):
        suppressed = False
        while self._exit_callbacks:
            cb = self._exit_callbacks.pop()
            try:
                if cb(exc_type, exc_value, traceback):
                    suppressed = True
                    exc_type = None
                    exc_value = None
                    traceback = None
            except BaseException as new_exc:
                exc_type = type(new_exc)
                exc_value = new_exc
                traceback = None
                suppressed = False
        if exc_value is not None and not suppressed:
            raise exc_value
        return suppressed

    def enter_context(self, cm):
        result = cm.__enter__()
        self._exit_callbacks.append(cm.__exit__)
        return result

    def callback(self, fn, *args, **kwargs):
        def _cb(exc_type, exc_value, traceback):
            fn(*args, **kwargs)
            return False
        self._exit_callbacks.append(_cb)
        return fn

    def push(self, exit_method):
        self._exit_callbacks.append(exit_method)
        return exit_method

    def pop_all(self):
        new_stack = ExitStack()
        new_stack._exit_callbacks = self._exit_callbacks
        self._exit_callbacks = []
        return new_stack

    def close(self):
        self.__exit__(None, None, None)
