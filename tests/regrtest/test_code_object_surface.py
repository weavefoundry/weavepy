# RFC 0033: CPython-faithful ``code`` object surface.
#
# Exercises the ``co_*`` attributes and methods a code object must
# expose so tooling (``dis``, ``inspect``, debuggers, coverage) can
# introspect compiled functions the same way it does under CPython.


def sample(x, y, z=10, *args, kw_only=None, **kwargs):
    total = x + y + z
    return total


co = sample.__code__

# ---------- counts ----------
assert co.co_argcount == 3, co.co_argcount
assert co.co_posonlyargcount == 0, co.co_posonlyargcount
assert co.co_kwonlyargcount == 1, co.co_kwonlyargcount
assert co.co_stacksize > 0, co.co_stacksize

# ---------- names ----------
assert co.co_name == "sample"
assert isinstance(co.co_qualname, str)
assert co.co_qualname.endswith("sample")

# co_varnames follows CPython's order: positional, keyword-only,
# *args, **kwargs, then locals.
vn = co.co_varnames
assert vn[:3] == ("x", "y", "z"), vn
assert vn[3] == "kw_only", vn  # keyword-only precedes *args
assert vn[4] == "args", vn
assert vn[5] == "kwargs", vn
assert "total" in vn, vn

# ---------- bytes-shaped fields ----------
assert isinstance(co.co_code, bytes), type(co.co_code)
assert len(co.co_code) > 0
assert len(co.co_code) % 2 == 0, "co_code is a 16-bit code-unit stream"
assert isinstance(co.co_linetable, bytes), type(co.co_linetable)
assert isinstance(co.co_exceptiontable, bytes), type(co.co_exceptiontable)

# ---------- consts / names tuples ----------
assert isinstance(co.co_consts, tuple)
assert isinstance(co.co_names, tuple)
assert isinstance(co.co_filename, str)
assert isinstance(co.co_firstlineno, int) and co.co_firstlineno > 0

# ---------- co_flags ----------
CO_VARARGS = 0x04
CO_VARKEYWORDS = 0x08
assert co.co_flags & CO_VARARGS, "sample declares *args"
assert co.co_flags & CO_VARKEYWORDS, "sample declares **kwargs"

# A plain function with neither must not set those bits.
def plain(a, b):
    return a + b


assert not (plain.__code__.co_flags & CO_VARARGS)
assert not (plain.__code__.co_flags & CO_VARKEYWORDS)

# ---------- co_lines() ----------
lines = list(co.co_lines())
assert len(lines) > 0
for start, end, lineno in lines:
    assert isinstance(start, int)
    assert isinstance(end, int)
    assert start <= end
    assert lineno is None or isinstance(lineno, int)

# ---------- co_positions() ----------
positions = list(co.co_positions())
assert len(positions) > 0
for pos in positions:
    assert len(pos) == 4, pos

# ---------- closures expose co_freevars / co_cellvars ----------
def make_counter():
    count = 0

    def inc():
        nonlocal count
        count += 1
        return count

    return inc


inc = make_counter()
assert "count" in inc.__code__.co_freevars
assert "count" in make_counter.__code__.co_cellvars
assert inc() == 1
assert inc() == 2

# ---------- nested code objects appear in co_consts ----------
nested = [c for c in make_counter.__code__.co_consts
          if hasattr(c, "co_name")]
assert any(c.co_name == "inc" for c in nested), "inner code object expected"

# ---------- replace() ----------
renamed = co.replace(co_name="renamed")
assert renamed.co_name == "renamed"
assert renamed.co_argcount == co.co_argcount
assert co.co_name == "sample", "replace() must not mutate the original"

print("test_code_object_surface: OK")
