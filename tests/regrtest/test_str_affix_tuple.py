"""`str.startswith` and `str.endswith` of a tuple of prefixes in compiled code.

Any matching prefix or suffix answers, non-ASCII text matches by
characters, an empty tuple never matches, an empty string always does,
and a start position, a non-`str` item or a subclass's receiver take the
full path, whose errors read as CPython's (a matching prefix ahead of a
bad item answers first, as there). Run it past the compile threshold.
"""


class S(str):
    pass


def run(n):
    for i in range(n):
        s = "héllo wörld"
        assert s.startswith(("x", "hé")) and not s.startswith(("x", "y"))
        assert s.endswith(("ld", "z")) and not s.endswith(())
        assert s.startswith(("",)) and s.endswith(("rld",))
        assert s.startswith(("wö", "zz"), 6) and not s.startswith(("h",), 1)
        assert S("abc").startswith(("a",)) and "abc".endswith(("bc", "x"))
        assert s.startswith(("h", 1))
        try:
            s.startswith(("x", 1))
        except TypeError as e:
            assert str(e) == "tuple for startswith must only contain str, not int", e
        else:
            raise AssertionError("int in the prefix tuple")
        try:
            s.endswith(2.5)
        except TypeError as e:
            assert str(e) == "endswith first arg must be str or a tuple of str, not float", e
        else:
            raise AssertionError("float suffix")


run(3000)
print("ok")
