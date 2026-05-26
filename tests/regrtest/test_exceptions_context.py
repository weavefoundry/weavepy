# RFC 0027 Group 5: Exception model + context management.
#
# Exercises PEP 678 (``BaseException.add_note`` / ``__notes__``),
# PEP 654 (``ExceptionGroup`` and ``except*``), the chained
# ``__cause__`` / ``__context__`` plumbing, ``contextlib`` builders
# (``contextmanager``, ``closing``, ``suppress``, ``ExitStack``,
# ``AsyncExitStack``), and ``contextvars`` (``Token.reset`` /
# ``copy_context``).
#
# The file is structured as a sequence of fail-fast asserts so the
# first divergence pinpoints the gap. Each block is independent.


# ---------- BaseException.add_note / __notes__ (PEP 678) ----------
err = ValueError("bad")
err.add_note("context: first call")
err.add_note("hint: try again later")
assert err.__notes__ == ["context: first call", "hint: try again later"]

# Notes survive ``raise`` and reach a handler.
try:
    raise err
except ValueError as caught:
    assert caught.__notes__ == ["context: first call", "hint: try again later"]
    caught.add_note("during handler")
    assert caught.__notes__[-1] == "during handler"


# ---------- __context__ / __cause__ chaining ----------
try:
    try:
        raise KeyError("inner")
    except KeyError as e:
        raise ValueError("outer") from e
except ValueError as caught:
    assert isinstance(caught.__cause__, KeyError)
    assert str(caught.__cause__) == "'inner'"
    assert caught.__suppress_context__ is True

try:
    try:
        raise KeyError("inner")
    except KeyError:
        raise ValueError("outer")
except ValueError as caught:
    assert isinstance(caught.__context__, KeyError)
    assert caught.__cause__ is None
    assert caught.__suppress_context__ is False


# ---------- ExceptionGroup (PEP 654) ----------
eg = ExceptionGroup("group", [ValueError("a"), TypeError("b"), ValueError("c")])
assert eg.message == "group"
assert len(eg.exceptions) == 3

matched, rest = eg.split(ValueError)
assert matched is not None
assert isinstance(matched, ExceptionGroup)
assert all(isinstance(e, ValueError) for e in matched.exceptions)
assert rest is not None
assert len(rest.exceptions) == 1
assert isinstance(rest.exceptions[0], TypeError)

sub = eg.subgroup(TypeError)
assert isinstance(sub, ExceptionGroup)
assert len(sub.exceptions) == 1

# ExceptionGroup.derive lets subclasses preserve their identity.
class CustomEG(ExceptionGroup):
    def derive(self, excs):
        return CustomEG(self.message, excs)

ce = CustomEG("custom", [ValueError("x"), TypeError("y")])
m2, _r = ce.split(ValueError)
assert isinstance(m2, CustomEG)
assert m2.message == "custom"


# ---------- contextlib.contextmanager ----------
import contextlib


@contextlib.contextmanager
def telemetry(label):
    events = [f"enter:{label}"]
    try:
        yield events
    finally:
        events.append(f"exit:{label}")


with telemetry("session") as e:
    e.append("doing work")

assert e == ["enter:session", "doing work", "exit:session"]


# ---------- contextlib.closing ----------
class Resource:
    def __init__(self):
        self.closed = False

    def close(self):
        self.closed = True


r = Resource()
with contextlib.closing(r):
    assert r.closed is False
assert r.closed is True


# ---------- contextlib.suppress ----------
with contextlib.suppress(KeyError):
    raise KeyError("ignored")

with contextlib.suppress(KeyError, ValueError):
    raise ValueError("also ignored")

caught_unrelated = None
try:
    with contextlib.suppress(KeyError):
        raise TypeError("not suppressed")
except TypeError as e:
    caught_unrelated = e
assert caught_unrelated is not None


# ---------- contextlib.ExitStack ----------
events = []


class Tracker:
    def __init__(self, name):
        self.name = name

    def __enter__(self):
        events.append(f"enter:{self.name}")
        return self

    def __exit__(self, *exc):
        events.append(f"exit:{self.name}")
        return False


with contextlib.ExitStack() as stack:
    stack.enter_context(Tracker("a"))
    stack.enter_context(Tracker("b"))
    stack.callback(lambda: events.append("callback:c"))

assert events == [
    "enter:a",
    "enter:b",
    "callback:c",
    "exit:b",
    "exit:a",
], events


# ExitStack.push_async_callback would need to await; we skip async
# stack here (covered separately under asyncio).


# ---------- contextvars ----------
import contextvars

ctx_var = contextvars.ContextVar("ctx_var", default="root")
assert ctx_var.get() == "root"

token = ctx_var.set("inside")
assert ctx_var.get() == "inside"
ctx_var.reset(token)
assert ctx_var.get() == "root"

ctx = contextvars.copy_context()


def run_in_ctx():
    ctx_var.set("from-ctx")
    return ctx_var.get()


inner_value = ctx.run(run_in_ctx)
assert inner_value == "from-ctx"
assert ctx_var.get() == "root"


# ---------- nested try/except with re-raise ----------
def chain_demo():
    try:
        raise ValueError("v")
    except ValueError as e:
        try:
            raise KeyError("k") from e
        except KeyError as k:
            return k


k = chain_demo()
assert isinstance(k, KeyError)
assert isinstance(k.__cause__, ValueError)


# ---------- assert with message ----------
try:
    assert False, "expected failure"
except AssertionError as e:
    assert str(e) == "expected failure"


print("test_exceptions_context: OK")
