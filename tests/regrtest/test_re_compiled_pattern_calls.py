"""`re` module functions called with an already compiled pattern.

`re.match(pat, s)` and its siblings use the pattern as it is when the
flags are empty (`0`, `False` or `re.NOFLAG`), and raise `ValueError` for
any other flags, as CPython's `_compile` does. Strings and bytes patterns
still go through the cache.
"""

import re

SECTCRE = re.compile(r"\[(?P<header>.+)\]")
WORDS = re.compile(rb"\w+")


def run(n):
    for _ in range(n):
        assert re.match(SECTCRE, "key") is None
        m = re.match(SECTCRE, "[sec]")
        assert m and m.group("header") == "sec" and m.re is SECTCRE
        assert re.search(SECTCRE, "x [a] y").span() == (2, 5)
        assert re.fullmatch(SECTCRE, "[a]", re.NOFLAG)
        assert re.match(SECTCRE, "[a]", False)
        assert re.sub(SECTCRE, "<>", "[a] [b]") == "<>"
        assert re.findall(WORDS, b"ab cd") == [b"ab", b"cd"]
        assert re.split(re.compile(","), "a,b") == ["a", "b"]
        assert [m.group() for m in re.finditer(WORDS, b"x y")] == [b"x", b"y"]
        assert re.compile(SECTCRE) is SECTCRE
        assert re.match(r"\d+", "12").group() == "12"
        for flags in (re.I, 2, True):
            try:
                re.match(SECTCRE, "[a]", flags)
            except ValueError:
                pass
            else:
                raise AssertionError(flags)


run(2000)
print("ok")
