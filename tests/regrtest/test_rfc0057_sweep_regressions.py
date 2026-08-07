"""RFC 0057 WS11 — engine fixes surfaced by the full-sweep regression grade.

Three distinct bugs, one bundled canary each:

1. `staticmethod`-wrapped C functions (`object.__new__`,
   `str.maketrans`) must report `builtin_function_or_method` — not
   `method_descriptor` — while keeping their descriptor metadata
   (inspect's `_NonUserDefinedCallables` gate; test_warnings'
   deprecated-class signature resolution).

2. VM-internal machinery loads (`module.__repr__` lazily reaching for
   `importlib._bootstrap`) must not route their import statements
   through a user-patched `builtins.__import__` — in CPython the
   bootstrap chain is frozen and initialized before user code runs
   (test_unittest: mock-patched discovery clobbered
   `sys.modules['sys']`, breaking output buffering suite-wide).

3. `faulthandler.register(chain=True)` needs `SA_NODEFER` so the
   chained `raise()` delivers the previous handler synchronously
   instead of looping on the re-installed handler forever
   (test_faulthandler test_register_chain hang).
"""

import sys
import types
import builtins

# ------------------- 1. staticmethod-wrapped builtins -------------------

assert type(object.__new__).__name__ == 'builtin_function_or_method', \
    type(object.__new__)
assert type(str.maketrans).__name__ == 'builtin_function_or_method', \
    type(str.maketrans)
# Descriptor metadata survives the classification.
assert object.__new__.__qualname__ == 'object.__new__'
assert str.maketrans.__qualname__ == 'str.maketrans'

# inspect.signature on a @deprecated class must keep resolving through
# the inherited user __init__ (the Cls7 shape from test_warnings).
import inspect
from warnings import deprecated


class _Base:
    def __init__(self, x, y):
        pass


class _Child(_Base):
    pass


_original = inspect.signature(_Child)
_deprecated = deprecated("gone")(_Child)
assert inspect.signature(_deprecated) == _original, inspect.signature(_deprecated)


# ------------------- 2. hooked __import__ vs. machinery loads -----------

_fake = types.ModuleType('package')


def _hijack(name, *args, **kwargs):
    sys.modules[name] = _fake
    return _fake


_real_import = builtins.__import__
builtins.__import__ = _hijack
try:
    # module repr lazily loads importlib._bootstrap; its internal
    # imports must not reach the hook.
    _r = repr(types.ModuleType('freshmod'))
finally:
    builtins.__import__ = _real_import
sys.modules.pop('package', None)

assert sys.modules['sys'] is sys, "machinery import leaked through the hook"
assert _r == "<module 'freshmod'>", _r

# The canonical downstream symptom: sys.stdout swaps must still be
# honored by print after the hook episode.
import io

_buf = io.StringIO()
_old_stdout = sys.stdout
sys.stdout = _buf
try:
    print('probe')
finally:
    sys.stdout = _old_stdout
assert _buf.getvalue() == 'probe\n', repr(_buf.getvalue())


# ------------------- 3. faulthandler chain=True ------------------------

if sys.platform != 'win32':
    import faulthandler
    import os
    import signal

    _called = []

    def _prev_handler(signum, frame):
        _called.append(signum)

    signal.signal(signal.SIGUSR1, _prev_handler)
    with open(os.devnull, 'w') as _sink:
        faulthandler.register(signal.SIGUSR1, file=_sink, chain=True)
        try:
            # Without SA_NODEFER this loops on the re-installed handler
            # forever (never returns) instead of chaining once.
            os.kill(os.getpid(), signal.SIGUSR1)
            assert _called == [signal.SIGUSR1], _called
        finally:
            faulthandler.unregister(signal.SIGUSR1)
            signal.signal(signal.SIGUSR1, signal.SIG_DFL)

print('ok')
