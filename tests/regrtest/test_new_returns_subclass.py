"""`__new__` returning an instance of a subclass, or of another class.

`type.__call__` runs `type(obj).__init__` when `__new__` returns an
instance of the class called or of a subclass (`pathlib.Path.__new__`
makes a `PosixPath`), and no `__init__` at all for anything else. Compiled
code chains the subclass's own `__init__` directly. Run it past the compile
threshold.
"""

import pathlib

log = []


class Base:
    def __new__(cls, *args, **kwargs):
        if cls is Base:
            cls = Sub
        return object.__new__(cls)

    def __init__(self, x):
        log.append(("base", x))


class Sub(Base):
    def __init__(self, x):
        log.append(("sub", x))


class Other:
    def __init__(self, *a):
        log.append(("other",))


class Odd:
    def __new__(cls, x):
        return Other()

    def __init__(self, x):
        log.append(("odd", x))


for i in range(3000):
    log.clear()
    b = Base(i)
    assert type(b) is Sub and log == [("sub", i)], log
    log.clear()
    s = Sub(i)
    assert type(s) is Sub and log == [("sub", i)], log
    log.clear()
    o = Odd(i)
    assert type(o) is Other and log == [("other",)], log
    p = pathlib.Path("/usr/local") / "lib" / "f.txt"
    assert str(p) == "/usr/local/lib/f.txt" and p.suffix == ".txt"
