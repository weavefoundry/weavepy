"""CPython's `_contextvars` accelerator name, aliased to WeavePy's
pure-Python `contextvars` implementation.

In CPython, `contextvars.py` is `from _contextvars import *`; here the
relationship is inverted (the Python module owns the per-thread state
and the C-API bridge), so the accelerator name must hand out the *same*
classes. 3.14's `threading` runs every thread body through
`_contextvars.Context().run(...)` and `_py_warnings` keeps its
catch_warnings state in a `_contextvars.ContextVar`; a separate type
here would leave threads silently never running their target.
"""

from contextvars import Context, ContextVar, Token, copy_context

__all__ = ["Context", "ContextVar", "Token", "copy_context"]
