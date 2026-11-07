"""A raise out of a compiled callee keeps its traceback, context and
handler semantics when the caller's compiled code catches it."""
import functools
import sys
import traceback


def thrower(n):
    raise ValueError(n)


def middle(n):
    return thrower(n) + 1


def catch_direct(n):
    try:
        thrower(n)
    except ValueError as error:
        return error, sys.exc_info()[1] is error, error.__traceback__
    return None


def catch_nested(n):
    try:
        middle(n)
    except ValueError as error:
        return error.__traceback__
    return None


def context_chain(n):
    try:
        raise KeyError(n)
    except KeyError:
        try:
            thrower(n)
        except ValueError as error:
            return error
    return None


def uncaught(n):
    thrower(n)


def caller_of_uncaught(n):
    try:
        uncaught(n)
    except ValueError as error:
        return error.__traceback__
    return None


def self_handled(n):
    try:
        raise ValueError(n)
    except ValueError:
        return n * 2


def calls_self_handled(n):
    try:
        return self_handled(n)
    except ValueError:
        return -1


class Boom:
    def __init__(self, n):
        raise TypeError(n)


def construct(n):
    try:
        Boom(n)
    except TypeError as error:
        return error.args[0]
    return None


@functools.lru_cache(maxsize=None)
def cached_thrower(n):
    raise LookupError(n)


def cached(n):
    try:
        cached_thrower(n)
    except LookupError as error:
        return error.args[0]
    return None


def names(tb):
    return [(frame.name, frame.lineno) for frame in traceback.extract_tb(tb)]


THROW_LINE = thrower.__code__.co_firstlineno + 1
MIDDLE_LINE = middle.__code__.co_firstlineno + 1

# Past the frame JIT's threshold, so later rounds run compiled code.
for i in range(3000):
    error, current, tb = catch_direct(i)
    assert error.args == (i,) and current, (error, current)
    assert names(tb) == [
        ("catch_direct", catch_direct.__code__.co_firstlineno + 2),
        ("thrower", THROW_LINE),
    ], names(tb)
    assert tb.tb_frame.f_code is catch_direct.__code__
    assert tb.tb_next.tb_frame.f_code is thrower.__code__
    assert tb.tb_next.tb_frame.f_locals == {"n": i}
    assert names(catch_nested(i)) == [
        ("catch_nested", catch_nested.__code__.co_firstlineno + 2),
        ("middle", MIDDLE_LINE),
        ("thrower", THROW_LINE),
    ]
    chained = context_chain(i)
    assert isinstance(chained.__context__, KeyError), chained.__context__
    assert chained.__context__.args == (i,)
    assert names(caller_of_uncaught(i)) == [
        ("caller_of_uncaught", caller_of_uncaught.__code__.co_firstlineno + 2),
        ("uncaught", uncaught.__code__.co_firstlineno + 1),
        ("thrower", THROW_LINE),
    ]
    assert calls_self_handled(i) == i * 2
    assert construct(i) == i
    assert cached(i % 7) == i % 7
    assert sys.exc_info() == (None, None, None), sys.exc_info()
print("callee raise fast paths: ok")
