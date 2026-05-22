"""Smoke test: exception hierarchy + traceback machinery + ExceptionGroup."""

# Basic catching by type and base class.
try:
    raise ValueError("hi")
except Exception as e:
    assert isinstance(e, ValueError)
    assert str(e) == "hi"

# Multiple exception types in one except.
for cls in (KeyError, ValueError):
    try:
        if cls is KeyError:
            raise KeyError("k")
        raise cls("v")
    except (KeyError, ValueError) as e:
        assert isinstance(e, cls)

# Custom exception subclass.
class MyError(RuntimeError):
    pass

try:
    raise MyError("boom")
except RuntimeError as e:
    assert isinstance(e, MyError)
    assert str(e) == "boom"

# Re-raise preserves the original.
def reraise():
    try:
        int("not a number")
    except ValueError:
        raise

try:
    reraise()
except ValueError as e:
    assert "invalid literal" in str(e)

# Chaining.
try:
    try:
        raise KeyError("orig")
    except KeyError as ke:
        raise RuntimeError("wrapped") from ke
except RuntimeError as e:
    assert e.__cause__ is not None
    assert isinstance(e.__cause__, KeyError)

# Context-only chain (implicit).
try:
    try:
        raise KeyError("inner")
    except KeyError:
        raise RuntimeError("outer")
except RuntimeError as e:
    assert e.__context__ is not None
    assert isinstance(e.__context__, KeyError)

# ExceptionGroup (PEP 654).
try:
    raise ExceptionGroup(
        "things broke",
        [ValueError("v"), KeyError("k"), TypeError("t")],
    )
except* ValueError as g:
    vs = g.exceptions
    assert len(vs) == 1 and isinstance(vs[0], ValueError)
except* (KeyError, TypeError) as g:
    others = g.exceptions
    assert len(others) == 2
