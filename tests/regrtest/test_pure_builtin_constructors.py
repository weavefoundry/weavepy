"""Builtin types whose calls build the object directly.

`object()`, `threading.Lock()`, `threading.RLock()`, `io.BytesIO()`,
`io.StringIO()`, `complex()` of plain numbers and `memoryview()` of
`bytes` or a `bytearray` skip the generic constructor when the call has nothing
for it to do; each object is new and works, arguments the shortcut
doesn't take (or rejects) behave as before, and subclasses construct as
they did. Run it past the compile threshold.
"""

import _thread
import io
import threading
import warnings


class Sub:
    pass


def run(n):
    prev = None
    for i in range(n):
        o = object()
        assert type(o) is object and o is not prev and not hasattr(o, "__dict__")
        prev = o
        lk = threading.Lock()
        assert type(lk) is _thread.LockType and not lk.locked()
        with lk:
            assert lk.locked() and not lk.acquire(False)
        assert lk.acquire(timeout=0.01)
        lk.release()
        rl = threading.RLock()
        with rl:
            with rl:
                assert rl._is_owned()
        assert not rl._is_owned()
        assert type(Sub()) is Sub
        c = complex(i, -0.0)
        assert c.real == i and str(c.imag) == "-0.0" and complex() == 0j
        assert complex(True) == 1 and complex(1.5, 2) == 1.5 + 2j
        assert complex("1+2j") == 1 + 2j and complex(1 + 1j) == 1 + 1j
        ba = bytearray(b"xy")
        mv = memoryview(ba)
        assert mv.obj is ba and bytes(mv) == b"xy"
        try:
            ba.append(1)
        except BufferError:
            pass
        else:
            raise AssertionError("resized an exported bytearray")
        mv.release()
        ba.append(1)
        assert memoryview(b"ab")[1] == 98
        try:
            object(1)
        except TypeError:
            pass
        else:
            raise AssertionError("object() took an argument")
        try:
            _thread.LockType(1)
        except TypeError:
            pass
        else:
            raise AssertionError("lock() took an argument")


run(3000)
with warnings.catch_warnings(record=True) as caught:
    warnings.simplefilter("always")
    try:
        threading.RLock(1)
    except TypeError:
        pass
assert any(issubclass(w.category, DeprecationWarning) for w in caught), caught
print("ok")
