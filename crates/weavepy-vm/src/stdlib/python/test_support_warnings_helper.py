"""``test.support.warnings_helper`` — warning-capture helpers.

Faithful subset of CPython 3.13's
``Lib/test/support/warnings_helper.py``: ``check_warnings``,
``check_no_resource_warning``, ``ignore_warnings``,
``save_restore_warnings_filters`` and the ``_filterwarnings`` engine the
first two share.
"""

import contextlib
import functools
import re
import sys
import warnings


@contextlib.contextmanager
def check_no_resource_warning(testcase):
    """Assert no ``ResourceWarning`` is raised inside the block."""
    with warnings.catch_warnings(record=True) as warns:
        warnings.filterwarnings('always', category=ResourceWarning)
        yield
    for w in warns:
        if issubclass(w.category, ResourceWarning):
            testcase.fail(f'Unexpected ResourceWarning: {w.message}')


def ignore_warnings(*, category):
    """Decorator silencing a warning *category* around a callable."""
    def decorator(test):
        @functools.wraps(test)
        def wrapper(self, *args, **kwargs):
            with warnings.catch_warnings():
                warnings.simplefilter('ignore', category=category)
                return test(self, *args, **kwargs)
        return wrapper
    return decorator


class WarningsRecorder:
    """Convenience wrapper over a recorded warnings list.

    Attribute access falls through to the *most recent* warning, so
    ``w.category`` / ``w.message`` work like CPython's recorder.
    """

    def __init__(self, warnings_list):
        self._warnings = warnings_list
        self._last = 0

    def __getattr__(self, attr):
        if len(self._warnings) > self._last:
            return getattr(self._warnings[-1], attr)
        elif attr in warnings.WarningMessage._WARNING_DETAILS:
            return None
        raise AttributeError("%r has no attribute %r" % (self, attr))

    @property
    def warnings(self):
        return self._warnings[self._last:]

    def reset(self):
        self._last = len(self._warnings)


def _filterwarnings(filters, quiet=False):
    """Catch warnings and check they match *filters* (CPython shape)."""
    frame = sys._getframe(2) if hasattr(sys, "_getframe") else None
    registry = frame.f_globals.get('__warningregistry__') if frame else None
    if registry:
        registry.clear()
    with warnings.catch_warnings(record=True) as w:
        warnings.resetwarnings()
        warnings.simplefilter("always")
        yield WarningsRecorder(w)
        # Match recorded warnings against the filters.
        reraise = list(w)
        missing = []
        for msg, cat in filters:
            seen = False
            for exc in reraise[:]:
                message = str(exc.message)
                if re.compile(msg).search(message) and (
                        cat is None or issubclass(exc.category, cat)):
                    seen = True
                    reraise.remove(exc)
            if not seen and not quiet:
                missing.append((msg, cat.__name__ if cat else None))
        if reraise and not quiet:
            raise AssertionError("unhandled warning %s" % reraise[0])
        if missing:
            raise AssertionError("filter (%r, %s) did not catch any warning" %
                                 missing[0])


@contextlib.contextmanager
def check_warnings(*filters, quiet=True):
    """Context manager wrapping ``warnings.catch_warnings``.

    With no *filters* it simply records and is forgiving (``quiet=True``);
    with ``(message_regex, category)`` pairs it asserts each is seen.
    """
    if not filters:
        filters = (("", None),)
        quiet = True
    yield from _filterwarnings(filters, quiet)


@contextlib.contextmanager
def save_restore_warnings_filters():
    old_filters = warnings.filters[:]
    try:
        yield
    finally:
        warnings.filters[:] = old_filters


def _warn_about_deprecation():  # pragma: no cover - probe helper
    warnings.warn(
        "This is a deprecation warning.",
        DeprecationWarning,
        stacklevel=1,
    )
