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
