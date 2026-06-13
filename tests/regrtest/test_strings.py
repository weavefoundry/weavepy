"""Smoke test: str / bytes methods and formatting."""

s = "hello world"
assert s.upper() == "HELLO WORLD"
assert s.lower() == "hello world"
assert s.capitalize() == "Hello world"
assert s.title() == "Hello World"
assert s.split() == ["hello", "world"]
assert s.split("o") == ["hell", " w", "rld"]
assert "-".join(["a", "b", "c"]) == "a-b-c"
assert s.replace("world", "there") == "hello there"
assert s.startswith("hello")
assert s.endswith("world")
assert s.find("world") == 6
assert s.count("l") == 3

assert "  hi  ".strip() == "hi"
assert "  hi  ".lstrip() == "hi  "
assert "  hi  ".rstrip() == "  hi"

assert "{} {}".format(1, 2) == "1 2"
assert "{0}/{1}/{0}".format("a", "b") == "a/b/a"
assert "{name}={val}".format(name="x", val=42) == "x=42"
assert "%d/%s" % (7, "ok") == "7/ok"

x = 42
assert f"x is {x}" == "x is 42"
assert f"{x:04d}" == "0042"
assert f"{x:>5}" == "   42"
assert f"{x:#x}" == "0x2a"

b = b"hello"
assert b.upper() == b"HELLO"
assert b + b" world" == b"hello world"
assert b.decode("utf-8") == "hello"
assert "hello".encode("utf-8") == b"hello"

assert len("abc") == 3
assert "abc"[1] == "b"
assert "abcdef"[1:4] == "bcd"
assert "abcdef"[1:] == "bcdef"
assert "abcdef"[:4] == "abcd"

# RFC 0037 (WS2): octal string/bytes escapes `\ooo` (1-3 octal digits).
assert "\101" == "A"
assert "\0" == "\x00"
assert "\7" == "\x07"
assert "\141\142" == "ab"
assert "\12" == "\n"
assert "\777" == "\u01ff"  # str allows values up to 0o777 (511)
assert b"\101" == b"A"
assert b"\377" == bytes([255])
assert b"\400" == bytes([0])  # bytes wrap mod 256
assert ord("\N{GREEK SMALL LETTER ALPHA}") == 0x3B1

# RFC 0037 (WS2): PEP 3131 non-ASCII identifiers (XID_Start / XID_Continue).
π = 3
assert π * 2 == 6
名前 = "weave"
assert 名前 == "weave"
Δt = 5
Δt += 1
assert Δt == 6
def σ(xs):
    total = 0
    for x in xs:
        total += x
    return total
assert σ([1, 2, 3]) == 6

# RFC 0037 (WS2b): PEP 701 f-strings — quote reuse, nesting, multiline
# expressions, backslashes, comments, and richer debug forms.
_d = {"a": 1, "b": 2}
assert f"{_d["a"]}/{_d["b"]}" == "1/2"          # same-quote subscript
_n = 3
assert f"{f"{_n * _n}"}" == "9"                  # nested f-string, same quote
assert f"{
    _n + 1
}" == "4"                                        # multiline replacement field
_t = {"k\t": 7}
assert f"{_d["a"]}{_t["k\t"]}" == "17"           # backslash in nested string
assert f"{1 + 2  # inline comment
}" == "3"                                        # comment inside field
_val = 7
assert f"{_val = }" == "_val = 7"                # debug form preserves spaces
assert f"{_val=}" == "_val=7"
_pi = 3.14159
assert f"{_pi = :.2f}" == "_pi = 3.14"           # debug form + format spec
assert f"{255:#x}" == "0xff"                     # `#` is literal in format spec
_w = 6
assert f"{_pi:.{_w}f}" == "3.141590"             # nested field in format spec
assert rf"\d{_n}\w" == "\\d3\\w"                 # raw f-string

print("strings ok")
