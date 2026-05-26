"""CPython-compatible `builtins` module.

In CPython this is the dict that backs every frame's `__builtins__`.
WeavePy registers the same dict ambiently at frame creation; the
import here just re-exposes those names as attributes of a real
module object so that callers like `pickle._find_class("builtins",
"len")` work.

We can't `from <somewhere> import *`, so we walk the running frame's
builtins dictionary at import time and stamp each entry as a module
attribute. The set of names is intentionally lazy: anything not
already in `__builtins__` simply won't appear here.
"""

import sys as _sys


def _populate():
    # Reach into the frame whose `f_builtins` we want to copy. Using
    # `_getframe(0)` returns this module's frame; `f_builtins` is the
    # dict the VM populated with `default_builtins()`.
    frame = _sys._getframe(0)
    src = frame.f_builtins
    mod = _sys.modules[__name__]
    for k, v in src.items():
        try:
            setattr(mod, k, v)
        except Exception:
            # Some names (e.g. `__builtins__` itself) can't be
            # round-tripped; skip them silently.
            pass


_populate()
del _populate
