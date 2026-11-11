"""Module attribute reads and module function calls from compiled code
(`os.path`, `mod.f(x)`, a module held in a local) see every change to the
module: a value replaced or deleted between reads, a new name added, a
module-level `__getattr__` (PEP 562) answering a missing name, and a
different module object reaching the same site."""

import sys
import types

mod = types.ModuleType("m_sites")
mod.K = 1
mod.f = lambda x: x + 1
sys.modules["m_sites"] = mod

other = types.ModuleType("m_other")
other.K = 100
other.f = lambda x: x * 10


def reads(m, n):
    t = 0
    for i in range(n):
        t += m.K + m.f(i)
    return t


def via_global(n):
    t = 0
    for i in range(n):
        t += mod.K + mod.f(1)
    return t


for _ in range(3):
    assert reads(mod, 3000) == 3000 + sum(range(1, 3001))
    assert via_global(3000) == 3000 * 3

mod.K = 5
mod.f = lambda x: -x
assert reads(mod, 3000) == 5 * 3000 - sum(range(3000))
assert via_global(3000) == 3000 * 4

assert reads(other, 300) == 100 * 300 + 10 * sum(range(300))

mod.EXTRA = 7
assert reads(mod, 100) == 5 * 100 - sum(range(100))

del mod.K


def missing(m, n):
    seen = 0
    for i in range(n):
        try:
            m.K
        except AttributeError:
            seen += 1
    return seen


assert missing(mod, 2000) == 2000
mod.__getattr__ = lambda name: 42 if name == "K" else None
assert reads(mod, 1000) == 42 * 1000 - sum(range(1000))
del mod.__getattr__
mod.K = 2
assert reads(mod, 1000) == 2 * 1000 - sum(range(1000))
print("ok")
