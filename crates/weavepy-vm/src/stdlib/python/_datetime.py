"""CPython's `_datetime` C accelerator, backed by the pure-Python
implementation.

WeavePy has no C datetime, but `_datetime` must be importable: `datetime.py`
prefers it, `test_types` imports it at module scope for `datetime_CAPI`, and
datetimetester's type-cache script re-imports it in a loop. Everything is
re-exported from `_pydatetime`, so the class objects are identical whichever
module a caller imports — and when a test harness *blocks* `_pydatetime`
(test_datetime's _Fast lane), importing this module fails too, which keeps
that lane cleanly skipped exactly like a build without the C accelerator.
"""

from _pydatetime import *          # noqa: F401,F403
from _pydatetime import __doc__    # noqa: F401


from _weave_capsule import new_capsule as _new_capsule

datetime_CAPI = _new_capsule("datetime.datetime_CAPI")
del _new_capsule

# The C module's classmethods are Argument Clinic `classmethod_descriptor`s
# publishing `__text_signature__`/`__objclass__` — pydoc's summary line for
# the *raw* `datetime.__dict__['utcnow']` reads both (test_pydoc
# test_unbound_builtin_classmethod_noargs). The pure-Python classmethod
# forwards these to its wrapped function, so pin them there.
_utcnow_func = datetime.__dict__['utcnow'].__func__
_utcnow_func.__text_signature__ = '($type, /)'
_utcnow_func.__objclass__ = datetime
del _utcnow_func
