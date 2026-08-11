"""RFC 0062 WS2 — prove the sdist-built wrapt is the *compiled* one.

The row installs `wrapt==2.3.0` with `no_binary`, so pip built the wheel
from source. wrapt selects implementations in `wrapt.__wrapt__`: it
imports the C `_wrappers` extension and flips `_using_c_extension`,
silently falling back to the pure-Python `wrappers` module on
ImportError — this probe fails if that fallback was taken.
"""

import wrapt
import wrapt._wrappers as _wrappers  # ImportError = the extension didn't build
from wrapt import __wrapt__

assert __wrapt__._using_c_extension is True, "wrapt took the pure-Python fallback"
assert wrapt.BaseObjectProxy is _wrappers.ObjectProxy, (
    f"wrapt.BaseObjectProxy is {wrapt.BaseObjectProxy!r}, not the C type"
)
assert wrapt.FunctionWrapper is _wrappers.FunctionWrapper

# Exercise the C decorator machinery for real, not just identity checks.
@wrapt.decorator
def double(wrapped, instance, args, kwargs):
    return 2 * wrapped(*args, **kwargs)

@double
def add(a, b):
    return a + b

assert add(3, 4) == 14, add(3, 4)
assert isinstance(add, wrapt.FunctionWrapper)
assert add.__wrapped__(3, 4) == 7

# The public ObjectProxy subclasses the C base; attribute passthrough
# must run through the compiled slots.
class Thing:
    answer = 42

proxy = wrapt.ObjectProxy(Thing())
assert isinstance(proxy, _wrappers.ObjectProxy)
assert proxy.answer == 42
proxy.answer = 43
assert proxy.__wrapped__.answer == 43

print("wrapt sdist probe ok:", _wrappers.__file__)
