"""`io.BytesIO()` and `io.StringIO()` built without the generic constructor.

The exact types, empty or given plain `bytes` or `str`, are their
`__new__`'s stream: positioned at the start, writable, and of the right
type; other arguments, keywords and subclasses take the full
constructor. Run it past the compile threshold.
"""

import io


class B(io.BytesIO):
    pass


class S(io.StringIO):
    def extra(self):
        return "x"


def run(n):
    for i in range(n):
        b = io.BytesIO()
        assert type(b) is io.BytesIO and b.tell() == 0 and b.getvalue() == b""
        b.write(b"ab")
        assert b.getvalue() == b"ab"
        b2 = io.BytesIO(b"xyz")
        assert b2.tell() == 0 and b2.read(2) == b"xy" and b2.getvalue() == b"xyz"
        s = io.StringIO()
        assert type(s) is io.StringIO and s.getvalue() == ""
        s.write("a\nb")
        assert s.getvalue() == "a\nb"
        s2 = io.StringIO("l1\r\nl2")
        assert s2.read() == "l1\r\nl2" and s2.tell() == 6
        assert io.StringIO("q", newline=None).getvalue() == "q"
        assert io.StringIO("a\r\nb", None).getvalue() == "a\nb"
        assert io.BytesIO(bytearray(b"ba")).getvalue() == b"ba"
        assert io.BytesIO(initial_bytes=b"k").getvalue() == b"k"
        assert type(B()) is B and B(b"z").getvalue() == b"z"
        assert S("w").extra() == "x" and S("w").getvalue() == "w"
        try:
            io.StringIO(5)
        except TypeError:
            pass
        else:
            raise AssertionError("int initial value")
        try:
            io.BytesIO("s")
        except TypeError:
            pass
        else:
            raise AssertionError("str initial bytes")


run(3000)
print("ok")
