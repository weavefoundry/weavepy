"""Raising and catching exceptions: custom classes, re-raise, finally,
context managers, and EAFP dictionary/attribute lookups."""

WORK = 40000


class AppError(Exception):
    def __init__(self, code, msg):
        super().__init__(msg)
        self.code = code


class NotFound(AppError):
    pass


def lookup(d, k):
    try:
        return d[k]
    except KeyError:
        raise NotFound(404, "missing %s" % k) from None


def guarded(d, k):
    try:
        return lookup(d, k)
    except NotFound as e:
        return -e.code
    finally:
        d["calls"] = d.get("calls", 0) + 1


class Obj:
    a = 1


def bench(n):
    d = {i: i for i in range(0, 100, 2)}
    o = Obj()
    total = 0
    for i in range(n):
        total += guarded(d, i % 100)
        try:
            total += o.missing
        except AttributeError:
            total += 1
        try:
            int("x%d" % i)
        except ValueError as e:
            total += len(e.args)
    return total + d["calls"]
