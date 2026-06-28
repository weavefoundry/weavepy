"""RFC 0042 WS4 — `http.cookies` in-process fixture.

Exercises the verbatim-ported `http.cookies` `Morsel`/`SimpleCookie`: parsing a
`Cookie:` header, attribute handling, and the `Set-Cookie:` output formatting.
Pure, no network.
"""

from http.cookies import SimpleCookie, Morsel

# --- parse a Cookie header into morsels --------------------------------------
C = SimpleCookie()
C.load('chips=ahoy; vienna=finger')
assert C["chips"].value == "ahoy"
assert C["vienna"].value == "finger"

# --- set values + attributes, then render Set-Cookie output ------------------
C = SimpleCookie()
C["sid"] = "abc123"
C["sid"]["path"] = "/"
C["sid"]["max-age"] = 3600
C["sid"]["httponly"] = True
C["sid"]["samesite"] = "Lax"
out = C["sid"].OutputString()
assert "sid=abc123" in out
assert "Path=/" in out
assert "Max-Age=3600" in out
assert "HttpOnly" in out
assert "SameSite=Lax" in out

# --- quoting of values with special characters -------------------------------
C = SimpleCookie()
C["k"] = 'a,b;c d"e'
rendered = C.output(header="")
re_parsed = SimpleCookie()
re_parsed.load(rendered.strip())
assert re_parsed["k"].value == 'a,b;c d"e', re_parsed["k"].value

# --- Morsel basics -----------------------------------------------------------
m = Morsel()
m.set("name", "value", "value")
assert m.key == "name"
assert m.value == "value"
assert m.isReservedKey("path")
assert not m.isReservedKey("name")

# --- js_output produces a <script> wrapper -----------------------------------
C = SimpleCookie()
C["x"] = "1"
assert "<script" in C["x"].js_output().lower()

print("WS4 http.cookies fixture ok")
