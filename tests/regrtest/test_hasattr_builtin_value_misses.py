"""`hasattr` and `getattr` with a default on values of built-in types
answer repeated misses the same as the first, without confusing a value
with its type, a subclass instance, or a hit."""

values = [None, True, 1, 1.5, "s", b"b", (1,), [1], {1: 2}, {1}, frozenset({1})]
for _ in range(200):
    for v in values:
        assert not hasattr(v, "__get__")
        assert not hasattr(v, "no_such_attribute")
        assert getattr(v, "__set__", "dflt") == "dflt"
        assert hasattr(v, "__class__")
        assert hasattr(v, "__eq__")
        assert getattr(v, "__class__", None) is type(v)

for _ in range(200):
    assert hasattr(1, "real") and getattr(1, "imag", None) == 0
    assert hasattr([], "append") and getattr({}, "get", None) is not None
    assert hasattr("x", "join") and not hasattr("x", "append")
    assert hasattr((), "count") and not hasattr((), "append")

# A miss on a value is not a miss on its type, nor on a subclass's
# instances.
assert not hasattr(1, "mro")
assert hasattr(int, "mro")


class MyInt(int):
    pass


class Lazy(list):
    def __getattr__(self, name):
        return "lazy:" + name


m = MyInt(3)
m.no_such_attribute = "set"
lz = Lazy()
for _ in range(200):
    assert not hasattr(3, "no_such_attribute")
    assert getattr(m, "no_such_attribute") == "set"
    assert hasattr(m, "no_such_attribute")
    assert hasattr(lz, "no_such_attribute")
    assert getattr(lz, "__get__", None) == "lazy:__get__"
    assert getattr([], "__get__", None) is None

# Errors other than AttributeError still propagate, and a missing
# attribute without a default still raises with CPython's message.
try:
    getattr(1, "nope")
except AttributeError as e:
    assert str(e) == "'int' object has no attribute 'nope'", str(e)
else:
    raise AssertionError("expected AttributeError")
try:
    hasattr(1, 2)
except TypeError as e:
    assert "attribute name must be string" in str(e), str(e)
else:
    raise AssertionError("expected TypeError")

print("ok")
