"""`str.partition`, `str.rpartition` and `str.splitlines` on exact strings.

The pieces, the separator object returned as the middle item, the string
itself when the separator isn't found, every line boundary (`\\r\\n`
together, the ASCII controls, and the non-ASCII ones), `keepends`, and a
`str` subclass's results behave as in CPython.
"""


class S(str):
    pass


def run(n):
    for _ in range(n):
        s, sep = "key=value=x", "="
        p = s.partition(sep)
        assert p == ("key", "=", "value=x") and p[1] is sep
        r = s.rpartition(sep)
        assert r == ("key=value", "=", "x") and r[1] is sep
        miss = s.partition(";")
        assert miss == (s, "", "") and miss[0] is s
        rmiss = s.rpartition(";")
        assert rmiss == ("", "", s) and rmiss[2] is s
        assert "a,b".partition(",") == ("a", ",", "b")
        assert ",".partition(",") == ("", ",", "")
        assert "ab".partition("ab") == ("", "ab", "")
        assert S("x-y").partition("-") == ("x", "-", "y")
        assert type(S("x-y").partition("-")[0]) is str
        text = "a\nb\r\nc\rd\x0be\x0cf\x1cg\x1dh\x1ei"
        assert text.splitlines() == list("abcdefghi")
        assert text.splitlines(True)[:3] == ["a\n", "b\r\n", "c\r"]
        assert "x\n\ny\n".splitlines() == ["x", "", "y"]
        assert "x\n\ny\n".splitlines(keepends=True) == ["x\n", "\n", "y\n"]
        assert "".splitlines() == [] and "\n".splitlines() == [""]
        assert "é z\x85w".splitlines() == ["é", "z", "w"]
        assert S("p\nq").splitlines() == ["p", "q"]


run(2000)
for bad in ("", ):
    try:
        "abc".partition(bad)
    except ValueError:
        pass
    else:
        raise AssertionError("empty separator")
print("ok")
