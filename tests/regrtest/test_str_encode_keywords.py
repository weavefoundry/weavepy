"""`str.encode` with its arguments by keyword, in compiled code.

UTF-8 with a builtin error handler, by position or keyword, gives the
text's bytes; other codecs take the full path. An unknown keyword, a name
given twice, and a non-`str` encoding raise as CPython's do. Run it past
the compile threshold.
"""


def enc(s):
    return (
        s.encode(encoding="utf-8"),
        s.encode(errors="strict"),
        s.encode("utf-8", errors="ignore"),
        s.encode(encoding="latin-1", errors="strict"),
    )


def bad(s, **kw):
    try:
        s.encode(**kw)
    except TypeError as e:
        return str(e)
    return None


for _ in range(3000):
    assert enc("héllo") == (b"h\xc3\xa9llo",) * 3 + (b"h\xe9llo",)
    assert bad("x", foo=1) == "encode() got an unexpected keyword argument 'foo'"
    assert bad("x", encoding=1) == "encode() argument 'encoding' must be str, not int"
    try:
        "x".encode("utf-8", encoding="utf-8")
    except TypeError as e:
        assert "given by name ('encoding') and position (1)" in str(e)
    else:
        raise AssertionError("no raise")
    assert "a€".encode(encoding="utf8") == b"a\xe2\x82\xac"
