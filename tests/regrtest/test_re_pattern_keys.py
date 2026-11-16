"""Compiled patterns as dict keys.

`re._compile` probes its cache with `(type(p), p, flags)` for every
compiled pattern a module function is handed, and catches the
`KeyError`. A pattern hashes natively, and a miss raises in place.
Equal patterns that aren't the same object still find each other, and a
replaced `__hash__` is called. Run it past the compile threshold.
"""

import re

p = re.compile(r"\[(?P<h>.+)\]")
q = re.compile(r"\[(?P<h>.+)\]")
b = re.compile(rb"x+", re.I)
assert hash(p) == hash(q) and p == q


def probe(d, pat):
    try:
        return d[type(pat), pat, 0]
    except KeyError as e:
        assert e.args[0][1] is pat
        return None


d = {(type(p), p, 0): 1}
for _ in range(3000):
    assert probe(d, q) == 1
    assert probe(d, p) == 1
    assert probe(d, b) is None
    assert probe({}, p) is None
    assert re.match(p, "[x]").group("h") == "x"
    assert re.sub(b, b"-", b"aXXb") == b"a-b"

try:
    re.Pattern.__hash__ = lambda self: 7
except TypeError:
    pass
else:
    assert hash(p) == 7
    assert probe({(re.Pattern, p, 0): 2}, p) == 2
