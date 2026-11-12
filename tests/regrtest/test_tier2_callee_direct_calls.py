"""Small looping callees (a filtered comprehension's function, say) that
tier 2 compiles keep their semantics when compiled callers call them
directly."""
import sys


def parts(path):
    return [x for x in path.split("/") if x and x != "."]


class Parser:
    sep = "/"

    @classmethod
    def parse(cls, path):
        if not path:
            return "", "", []
        drv, root, rel = "", cls.sep if path.startswith(cls.sep) else "", path.lstrip(cls.sep)
        parsed = [sys.intern(str(x)) for x in rel.split(cls.sep) if x and x != "."]
        return drv, root, parsed


def total(xs):
    t = 0
    for x in xs:
        if x > 2:
            t += x
    return t


def outer(n):
    acc = 0
    for i in range(n):
        acc += len(parts("/a/./b/c%d" % i))
        drv, root, parsed = Parser.parse("/srv/./app/x%d" % i)
        acc += len(parsed) + len(root)
        acc += total(range(i % 7))
    return acc


def expected(n):
    acc = 0
    for i in range(n):
        acc += 3 + 4 + sum(x for x in range(i % 7) if x > 2)
    return acc


# Past both tiers' thresholds, so later rounds run compiled code.
for r in range(40):
    assert outer(500) == expected(500), (r, outer(500), expected(500))
assert parts("") == [] and parts("/./") == []
assert Parser.parse("") == ("", "", [])
try:
    parts(None)
except AttributeError:
    pass
else:
    raise AssertionError("parts(None) didn't raise")
print("tier-2 callee direct calls: ok")
