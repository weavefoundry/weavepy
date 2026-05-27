"""``exceptiongroup`` — PEP 654 ExceptionGroup back-port.

In Python ≥3.11 ``ExceptionGroup`` / ``BaseExceptionGroup`` are
built in; the third-party ``exceptiongroup`` package is a no-op
re-export there. We mirror that shape so user code that imports
``from exceptiongroup import ExceptionGroup`` works on top of
WeavePy's built-in implementation.
"""

try:
    BaseExceptionGroup = BaseExceptionGroup  # noqa: F811 - built-in (3.11+).
    ExceptionGroup = ExceptionGroup  # noqa: F811
except NameError:  # pragma: no cover - WeavePy ships built-in support.
    class BaseExceptionGroup(BaseException):
        def __new__(cls, message, exceptions):
            inst = super().__new__(cls, message, list(exceptions))
            inst.message = message
            inst.exceptions = tuple(exceptions)
            return inst

        def derive(self, excs):
            return BaseExceptionGroup(self.message, excs)

        def subgroup(self, condition):
            matched = [e for e in self.exceptions if condition(e)]
            if not matched:
                return None
            return self.derive(matched)

        def split(self, condition):
            matched, unmatched = [], []
            for e in self.exceptions:
                (matched if condition(e) else unmatched).append(e)
            return (self.derive(matched) if matched else None,
                    self.derive(unmatched) if unmatched else None)

    class ExceptionGroup(BaseExceptionGroup, Exception):
        pass


def catch(handlers):
    """Context manager that splits an ExceptionGroup by type.

    ``handlers`` maps exception classes (or tuples thereof) to
    callables that take the matched sub-group. Mirrors the
    ``exceptiongroup.catch`` 1.2+ surface.
    """
    return _CatchContext(handlers)


class _CatchContext:
    def __init__(self, handlers):
        self.handlers = handlers

    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc_val, tb):
        if exc_val is None or not isinstance(exc_val, BaseExceptionGroup):
            return False
        unmatched = exc_val
        for key, handler in self.handlers.items():
            if not isinstance(key, tuple):
                key = (key,)
            if unmatched is None:
                break
            matched, unmatched = unmatched.split(lambda e: isinstance(e, key))
            if matched is not None:
                try:
                    handler(matched)
                except BaseException:
                    raise
        if unmatched is not None:
            raise unmatched
        return True


__all__ = ['BaseExceptionGroup', 'ExceptionGroup', 'catch']
