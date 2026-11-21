"""`for entry in os.scandir(...)` in compiled code.

`glob` iterates `os.scandir()` in a `with` block. Compiled code and the
core loop step the iterator through its native `__next__` in place: the
entries are the directory's, an exhausted or closed iterator doesn't warn
when it goes, and an abandoned one still does. Run it past the compile
threshold.
"""

import os
import tempfile
import warnings

root = tempfile.mkdtemp()
for name in ("a.py", "b.txt", "c.py"):
    with open(os.path.join(root, name), "w"):
        pass
os.mkdir(os.path.join(root, "d"))
expected = sorted(os.listdir(root))


def names_with():
    with os.scandir(root) as it:
        return sorted(e.name for e in it)


def names_loop():
    out = []
    for e in os.scandir(root):
        out.append((e.name, e.is_dir()))
    return sorted(out)


def first_only():
    it = os.scandir(root)
    for e in it:
        break
    it.close()


with warnings.catch_warnings():
    warnings.simplefilter("error", ResourceWarning)
    for _ in range(3000):
        assert names_with() == expected
        assert [n for n, _ in names_loop()] == expected
        assert dict(names_loop())["d"] is True
        first_only()

with warnings.catch_warnings(record=True) as caught:
    warnings.simplefilter("always", ResourceWarning)
    it = os.scandir(root)
    next(it)
    del it
assert any(issubclass(w.category, ResourceWarning) for w in caught), caught
