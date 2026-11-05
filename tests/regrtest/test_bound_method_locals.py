"""A method cached in a local (`get = source.get`, as `re`, `json` and
`tokenize` do) and called from compiled code calls as the method does: the
right receiver, defaults, a method rebound on the class afterwards (the
cached bound method keeps the old function, as in CPython), a staticmethod
and a builtin's bound method, and the receiver's reference count back
where it was."""
import sys


class Tokens:
    def __init__(self, text):
        self.text = text
        self.i = 0

    def get(self):
        c = self.text[self.i]
        self.i += 1
        return c

    def match(self, c, step=1):
        if self.text[self.i] == c:
            self.i += step
            return True
        return False

    @staticmethod
    def kind(c):
        return "digit" if c.isdigit() else "other"


def scan(t, n):
    get, match, kind, out = t.get, t.match, t.kind, []
    append = out.append
    for _ in range(n):
        if match("x"):
            append("X")
        else:
            append(kind(get()))
    return out


def old_get(self):
    return "old"


# Past the frame JIT's threshold, so later rounds run compiled code.
for i in range(3000):
    t = Tokens("1x2ax" * 3)
    before = sys.getrefcount(t)
    assert scan(t, 9) == ["digit", "X", "digit", "other", "X", "digit", "X", "digit", "other"]
    assert t.i == 9
    assert sys.getrefcount(t) == before
    u = Tokens("ab")
    bound = u.get
    assert bound() == "a" and bound() == "b"

# The class's method rebound: a bound method made before keeps its function.
t = Tokens("abc")
get = t.get
Tokens.get = old_get
assert get() == "a" and t.get() == "old"
print("ok")
