"""RFC 0052 WS5 — patchable builtins.

`sys.modules['builtins'].__dict__` and the interpreter's ambient
lookup namespace are one dict: attribute writes, raw dict mutation,
and `unittest.mock.patch` must all be observed by `LOAD_GLOBAL` —
including in *already-specialized* code — and sandboxed `exec`
namespaces must stay sealed.
"""

import builtins
import sys
from unittest import mock

# --- module identity ---------------------------------------------------
assert sys.modules['builtins'] is builtins
assert builtins.__dict__ is vars(sys.modules['builtins'])
assert sys._getframe(0).f_builtins is builtins.__dict__
assert builtins.__name__ == 'builtins'

# --- raw dict mutation is live for name resolution ---------------------
def use_len(x):
    return len(x)

assert use_len([1, 2, 3]) == 3
orig_len = builtins.__dict__['len']
builtins.__dict__['len'] = lambda x: 42
try:
    assert use_len([1, 2, 3]) == 42
finally:
    builtins.__dict__['len'] = orig_len
assert use_len([1, 2, 3]) == 3

# --- attribute writes and deletes --------------------------------------
orig_abs = builtins.abs
builtins.abs = lambda x: 'patched'
try:
    assert abs(-5) == 'patched'
finally:
    builtins.abs = orig_abs
assert abs(-5) == 5

builtins.__dict__['_weave_tmp'] = 7
assert _weave_tmp == 7  # noqa: F821
del builtins.__dict__['_weave_tmp']
try:
    _weave_tmp  # noqa: F821
except NameError:
    pass
else:
    raise AssertionError('expected NameError after dict delete')

# --- mock.patch deopts already-specialized LOAD_GLOBAL ------------------
def hot():
    return len([1, 2])

for _ in range(300):  # warm the inline cache
    hot()
with mock.patch('builtins.len', lambda x: 'patched'):
    assert hot() == 'patched'
assert hot() == 2

with mock.patch('builtins.open', mock.mock_open(read_data='data')):
    with open('/nonexistent') as fh:
        assert fh.read() == 'data'

# --- func_builtins snapshots at function creation (CPython) -------------
# Rebinding globals()['__builtins__'] between calls must not change an
# existing function's resolution (test_dynamic's
# test_cannot_replace_builtins_dict_between_calls).
saved = globals()['__builtins__']
globals()['__builtins__'] = {'len': lambda x: 7}
try:
    assert use_len([1, 2, 3]) == 3
finally:
    globals()['__builtins__'] = saved

# --- sandboxed exec namespaces stay sealed ------------------------------
ns = {'__builtins__': {'len': lambda x: -1}}
exec("r = len([1,2,3])", ns)
assert ns['r'] == -1

try:
    exec("print('hi')", {'__builtins__': {}})
except NameError:
    pass
else:
    raise AssertionError('expected NameError from sealed builtins')

# A function *defined inside* the sandbox inherits the sandbox.
ns2 = {'__builtins__': {'len': lambda x: -2}}
exec("def f():\n    return len([1])", ns2)
assert ns2['f']() == -2

# `__builtins__` may also be the module object itself (CPython allows both).
ns3 = {'__builtins__': builtins}
exec("r = len([1,2])", ns3)
assert ns3['r'] == 2

print('ok')
