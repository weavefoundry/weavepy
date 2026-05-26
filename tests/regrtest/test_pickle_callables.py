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

    print("pickle callables ok")


if __name__ == "__main__":
    main()
