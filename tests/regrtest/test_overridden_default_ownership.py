"""Overridden positional defaults retain ownership through argument binding."""
import gc
import weakref


class Token:
    pass


def select(a, b, c, d):
    return a, b, c, d


def exercise_partial_overrides():
    first, second, third, fourth = [Token() for _ in range(4)]
    defaults = first, second, third, fourth
    select.__defaults__ = defaults
    for _ in range(100):
        result = select(a=19, c=23)
        assert result[0] == 19 and result[2] == 23
        assert result[1] is second and result[3] is fourth
        assert select(*(), **{}) == defaults
    select.__defaults__ = (Token(), Token(), *defaults)
    assert select() == defaults
    select.__defaults__ = None
    try:
        select()
    except TypeError as error:
        assert "4 required positional arguments" in str(error)
    else:
        raise AssertionError("cleared defaults still bind")


def exercise_replacement_lifetime():
    def clear_from_body(value):
        clear_from_body.__defaults__ = None
        gc.collect()
        assert reference() is value
        return value

    value = Token()
    reference = weakref.ref(value)
    clear_from_body.__defaults__ = (value,)
    del value
    result = clear_from_body()
    assert reference() is result
    del result
    gc.collect()
    assert reference() is None


def exercise_code_replacement():
    def target(a, b, c, d):
        return a, b, c, d

    def shortened(left, /, right, *, flag=9, **extra):
        return left, right, flag, extra

    one, two = Token(), Token()
    target.__defaults__ = (Token(), Token(), one, two)
    target.__code__ = shortened.__code__
    target.__kwdefaults__ = {"flag": 9}
    for _ in range(100):
        result = target(left="extra", right=21)
        assert result[0] is one and result[1:] == (21, 9, {"left": "extra"})
        result = target()
        assert result[0] is one and result[1] is two
    target.__kwdefaults__ = None
    try:
        target()
    except TypeError as error:
        assert "keyword-only argument: 'flag'" in str(error)
    else:
        raise AssertionError("missing keyword-only argument accepted")


def exercise_bound_receiver():
    class Holder:
        def method(self, a, b, c):
            return self, a, b, c

    holder = Holder()
    one, two, three = Token(), Token(), Token()
    Holder.method.__defaults__ = (Token(), one, two, three)
    for _ in range(100):
        result = holder.method(b=29)
        assert result[0] is holder and result[1] is one
        assert result[2] == 29 and result[3] is three


exercise_partial_overrides()
exercise_replacement_lifetime()
exercise_code_replacement()
exercise_bound_receiver()
print("overridden positional defaults: ownership and binding ok")
