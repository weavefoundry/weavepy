"""A dict subclass's `__missing__` serves global-name misses for code run
under it as globals, for as long as that code can run.

Builtin lookups skip the check once no such globals are left (the
owners are pruned when nothing can reach them), so a function made
under them must keep them alive, and a later one must register again.
"""

import gc


class Fallback(dict):
    def __missing__(self, key):
        if key == "magic":
            return 42
        if key == "len":
            # Shadows the builtin: a miss asks the mapping first.
            return lambda x: 100
        raise KeyError(key)


def make(ns):
    exec("def f():\n    return magic + len('ab')\n", ns)
    return ns["f"]


f = make(Fallback())
assert f() == 142

# Unrelated builtin-heavy code in between, and collections, which prune
# the owners nothing reaches.
for _ in range(3):
    gc.collect()
    total = 0
    for i in range(2000):
        total += abs(-i) + len("xy")
    assert total == sum(range(2000)) + 4000

# `f` still holds its globals: the misses still go to `__missing__`, in
# whatever tier runs it once it's hot.
for _ in range(5000):
    assert f() == 142

# Drop it; a fresh subclass-globals run registers again.
del f
gc.collect()
g = make(Fallback())
for _ in range(5000):
    assert g() == 142

# eval with a subclass mapping as globals.
assert eval("magic * 2", Fallback()) == 84

# A dataclass whose annotations name an undefined type (annotationlib's
# fake globals) leaves builtin lookups working afterwards.
from dataclasses import dataclass


@dataclass
class P:
    x: "Undefined"  # noqa: F821
    y: int = 0


assert P(1).x == 1
gc.collect()
assert len([1, 2, 3]) == 3 and abs(-5) == 5
print("ok")
