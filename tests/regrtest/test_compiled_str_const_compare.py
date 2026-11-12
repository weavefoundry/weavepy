"""`==` and `!=` between a local and a string constant from compiled code
keep CPython's results: exact strings compare by content (the same object
or an equal copy), and anything else (a `str` subclass with its own
`__eq__`, an int, None, a non-ASCII or surrogate string) still takes the
full comparison."""


class Loud(str):
    calls = 0

    def __eq__(self, other):
        Loud.calls += 1
        return True

    __hash__ = str.__hash__


def classify(c):
    if c == "[":
        return 1
    if c != "x":
        return 2
    return 3


def count(src):
    n = [0, 0, 0, 0]
    for ch in src:
        n[classify(ch)] += 1
    return n


src = "[ab]x" * 2000
assert count(src) == [0, 2000, 6000, 2000]
built = "".join(["[", "x"])
assert classify(built[0]) == 1 and classify(built[1]) == 3
assert classify("") == 2 and classify("xx") == 2
assert classify("é") == 2 and classify("\ud800") == 2
assert classify(3) == 2 and classify(None) == 2
assert classify(Loud("q")) == 1 and Loud.calls == 1


def eq_pair(a, b):
    return a == b, a != b


for _ in range(3000):
    assert eq_pair("abc", "abc") == (True, False)
assert eq_pair("abc", "abd") == (False, True)
assert eq_pair("abc", 1) == (False, True)
print("ok")
