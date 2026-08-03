"""RFC 0026 — pickle round-trip of functions and classes by qualified name.

The multiprocessing spawn path relies on `pickle.dumps(target)` to
emit a `GLOBAL` opcode that names the function by `<module>.<qualname>`.
This test verifies the encoder/decoder pair in isolation.
"""

import pickle
import sys


def hello():
    return "hello"


class Counter:
    pass


class WithClassReduce:
    """Class-level `__reduce__` override (pickles as a fresh instance)."""

    def __reduce__(self):
        return (WithClassReduce, ())

    def _blocked_reduce(self):
        raise RuntimeError("pickling disabled for this instance")


def main():
    # --- module-level function -------------------------------------------
    blob = pickle.dumps(hello)
    restored = pickle.loads(blob)
    assert restored is hello
    assert restored() == "hello"

    # --- module-level class ---------------------------------------------
    blob = pickle.dumps(Counter)
    restored = pickle.loads(blob)
    assert restored is Counter

    # --- builtin function (re-resolved via builtins module) -------------
    blob = pickle.dumps(len)
    restored = pickle.loads(blob)
    assert restored is len
    assert restored([1, 2, 3]) == 3

    # --- primitive dict containing a function ---------------------------
    payload = {"fn": hello, "args": (1, 2)}
    restored = pickle.loads(pickle.dumps(payload))
    assert restored["fn"] is hello
    assert restored["args"] == (1, 2)

    # --- instance-dict __reduce__ override (RFC 0056 WS7) ----------------
    # CPython's object.__reduce_ex__ gates the override on the *class*
    # attribute but then calls PyObject_GetAttr(self, "__reduce__"), so a
    # plain instance-dict __reduce__ wins over the class method. zoneinfo's
    # ZoneInfo.from_file relies on this to forbid pickling file-born zones.
    obj = WithClassReduce()
    restored = pickle.loads(pickle.dumps(obj))
    assert type(restored) is WithClassReduce

    blocked = WithClassReduce()
    blocked.__reduce__ = blocked._blocked_reduce
    for proto in range(pickle.HIGHEST_PROTOCOL + 1):
        try:
            pickle.dumps(blocked, protocol=proto)
        except RuntimeError as exc:
            assert "pickling disabled" in str(exc)
        else:
            raise AssertionError(
                f"instance-dict __reduce__ ignored at protocol {proto}"
            )

    print("pickle callables ok")


if __name__ == "__main__":
    main()
