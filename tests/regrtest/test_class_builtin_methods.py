"""A builtin function kept on a class and called through an instance
(`converter = time.localtime`, `self.converter(t)`) is called unbound, as
CPython's builtin functions have no `__get__`: from compiled loops too,
with the class attribute replaced, an instance attribute shadowing it, a
`staticmethod` beside it, and a method descriptor (`list.append`) that
does bind."""

import time


class Fmt:
    conv = time.gmtime
    size = len

    @staticmethod
    def twice(x):
        return 2 * x

    def run(self, n):
        t = 0
        for i in range(n):
            t += self.conv(i * 3600).tm_hour
            t += self.size("abc")
            t += self.twice(1)
        return t


f = Fmt()
for _ in range(3):
    assert f.run(48) == sum((i % 24) for i in range(48)) + 48 * 3 + 48 * 2

Fmt.size = abs
try:
    f.run(1)
except TypeError as e:
    assert "abs()" in str(e) or "bad operand" in str(e), e
else:
    raise AssertionError("abs('abc') didn't raise")
Fmt.size = len

g = Fmt()
g.size = lambda s: 100
assert g.run(24) == sum(range(24)) + 24 * 100 + 24 * 2
assert f.run(24) == sum(range(24)) + 24 * 3 + 24 * 2


class Binds:
    app = list.append

    def run(self, n):
        seen = 0
        for i in range(n):
            try:
                self.app(i)
            except TypeError:
                seen += 1
        return seen


for _ in range(3):
    assert Binds().run(3000) == 3000
print("ok")
