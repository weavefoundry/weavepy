# `try` / `finally` — the bare form (no `except`) must run the finally
# body and propagate the original exception. Historically the
# compiler discarded the in-flight exception before running `finally`,
# which produced an internal `stack underflow` on the reraise. This
# fixture pins that down.


def with_finally():
    try:
        raise AttributeError("boom")
    finally:
        print("finally ran")


try:
    with_finally()
except AttributeError as e:
    print("caught:", e)


# A nested try/finally with a yielded coroutine that re-raises must
# also unwind cleanly. We use a generator + manual driver to keep the
# test free of any stdlib state machine assumptions.

def yields_then_raises():
    try:
        yield 1
        yield 2
        raise RuntimeError("late boom")
    finally:
        print("inner finally")


g = yields_then_raises()
print(next(g))
print(next(g))
try:
    next(g)
except RuntimeError as e:
    print("caught:", e)


# An exception that flies through several layers of try/finally
# should run each finally and still propagate.

def layered():
    try:
        try:
            raise ValueError("v")
        finally:
            print("inner")
    finally:
        print("outer")


try:
    layered()
except ValueError as e:
    print("caught:", e)
