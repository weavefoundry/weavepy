"""f-string conversions of strings and floats keep their semantics when
compiled code formats them."""


class S(str):
    def __repr__(self):
        return "S!"

    def __str__(self):
        return "S?"


class F(float):
    def __repr__(self):
        return "F!"


def conv(b, c, s):
    return f"{b!r}|{c!r}|{c}|{c!s}|{s!r}|{b!s}|{b}"


# Past the frame JIT's threshold, so later rounds run compiled code.
for _ in range(3000):
    assert conv("it's", 0.1, "a\nb") == "\"it's\"|0.1|0.1|0.1|'a\\nb'|it's|it's", conv("it's", 0.1, "a\nb")
    assert conv("q\"'", 1e100, "\x00é") == "'q\"\\''|1e+100|1e+100|1e+100|'\\x00é'|q\"'|q\"'"
    assert conv("", -0.0, "\U0001f600") == "''|-0.0|-0.0|-0.0|'\U0001f600'||"
    assert conv("x", float("nan"), "\\") == "'x'|nan|nan|nan|'\\\\'|x|x"
    assert conv("x", float("-inf"), "\u200b") == "'x'|-inf|-inf|-inf|'\\u200b'|x|x"
    assert conv(S("x"), F(2.5), S("y")) == "S!|F!|F!|F!|S!|S?|S?", conv(S("x"), F(2.5), S("y"))
print("f-string conversion fast paths: ok")
