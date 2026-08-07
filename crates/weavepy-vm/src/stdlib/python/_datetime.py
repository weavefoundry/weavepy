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


class PyCapsule:
    """Stand-in for CPython's opaque `PyCapsule` (the type behind
    `types.CapsuleType`). Not instantiable, like the real one."""

    __module__ = 'builtins'

    _capsule_name = "datetime.datetime_CAPI"

    def __new__(cls, *args, **kwargs):
        raise TypeError("cannot create 'PyCapsule' instances")

    def __repr__(self):
        return '<capsule object "%s" at %#x>' % (self._capsule_name, id(self))


datetime_CAPI = object.__new__(PyCapsule)
# Reachable as type(datetime_CAPI); keeping the name out of the module dict
# mirrors the C module's surface (only the capsule itself is exposed).
del PyCapsule
