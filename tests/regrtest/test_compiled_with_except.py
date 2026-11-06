"""A `with` block whose body raises, in compiled code (which calls the
manager's Python `__exit__` directly), keeps CPython's results: the
arguments `__exit__` sees, a suppressing and a propagating exit, a truthy
result that isn't a bool, a raise inside `__exit__` (with its context), a
generator-based manager, nested blocks, the traceback's lines, and a
builtin manager's exit."""
import contextlib
import threading
import traceback


class Mgr:
    def __init__(self, suppress):
        self.suppress = suppress
        self.seen = None

    def __enter__(self):
        return self

    def __exit__(self, et, ev, tb):
        self.seen = (et, type(ev), tb is not None)
        return self.suppress


class Bad:
    def __enter__(self):
        return self

    def __exit__(self, *a):
        raise RuntimeError("in exit")


@contextlib.contextmanager
def catching():
    try:
        yield
    except KeyError:
        pass


def body(i):
    raise KeyError(i)


def run(i):
    m = Mgr(True)
    with m:
        body(i)
    assert m.seen == (KeyError, KeyError, True)
    m = Mgr([1])
    with m:
        body(i)
    m = Mgr(0)
    try:
        with m:
            body(i)
    except KeyError as e:
        names = [f.name for f in traceback.extract_tb(e.__traceback__)]
        assert names[-2:] == ["run", "body"], names
    else:
        raise AssertionError("a propagating exit suppressed")
    try:
        with Bad():
            body(i)
    except RuntimeError as e:
        assert type(e.__context__) is KeyError
    else:
        raise AssertionError("the exit's raise was lost")
    with catching():
        body(i)
    outer = Mgr(True)
    with outer:
        with Mgr(False):
            body(i)
    assert outer.seen[0] is KeyError
    lock = threading.Lock()
    try:
        with lock:
            body(i)
    except KeyError:
        assert not lock.locked()
    with contextlib.suppress(KeyError):
        body(i)


# Past the frame JIT's threshold, so later rounds run compiled code.
for i in range(2000):
    run(i)
print("ok")
