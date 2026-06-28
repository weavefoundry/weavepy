"""RFC 0042 WS4 — `urllib.parse` in-process fixture.

Exercises the verbatim-ported `urllib.parse`: the `SplitResult`/`ParseResult`
named tuples really are `tuple` subclasses, and `urlsplit`/`urlunsplit`/
`urljoin`/`quote`/`unquote`/`parse_qs[l]`/`urlencode` round-trip. Pure, no
network.
"""

from urllib.parse import (
    urlsplit, urlunsplit, urlparse, urlunparse, urljoin,
    quote, unquote, quote_plus, unquote_plus,
    parse_qs, parse_qsl, urlencode, SplitResult, ParseResult,
)

# --- SplitResult / ParseResult are genuine tuple subclasses -----------------
sr = urlsplit("https://user:pass@host.example:8443/a/b?x=1&y=2#frag")
assert isinstance(sr, tuple), "SplitResult must subclass tuple"
assert isinstance(sr, SplitResult)
assert sr.scheme == "https"
assert sr.hostname == "host.example"
assert sr.port == 8443
assert sr.username == "user"
assert sr.password == "pass"
assert sr.path == "/a/b"
assert sr.query == "x=1&y=2"
assert sr.fragment == "frag"
# Tuple semantics: indexing, unpacking, equality with a plain tuple.
assert sr[0] == "https"
assert tuple(sr) == ("https", "user:pass@host.example:8443", "/a/b", "x=1&y=2", "frag")
scheme, netloc, path, query, frag = sr
assert netloc == "user:pass@host.example:8443"

# Round-trips.
assert urlunsplit(sr) == "https://user:pass@host.example:8443/a/b?x=1&y=2#frag"

pr = urlparse("http://example.com/p;params?q=v#f")
assert isinstance(pr, ParseResult) and isinstance(pr, tuple)
assert pr.params == "params"
assert urlunparse(pr) == "http://example.com/p;params?q=v#f"

# --- urljoin ----------------------------------------------------------------
assert urljoin("http://a/b/c/d;p?q", "g") == "http://a/b/c/g"
assert urljoin("http://a/b/c/d;p?q", "../g") == "http://a/b/g"
assert urljoin("http://a/b/c/d;p?q", "//g") == "http://g"
assert urljoin("http://a/b/c/d;p?q", "?y") == "http://a/b/c/d;p?y"

# --- quoting ----------------------------------------------------------------
assert quote("a b/c?") == "a%20b/c%3F"
assert quote("a b/c?", safe="") == "a%20b%2Fc%3F"
assert quote_plus("a b+c") == "a+b%2Bc"
assert unquote("a%20b%2Fc") == "a b/c"
assert unquote_plus("a+b%2Bc") == "a b+c"
assert unquote("%E2%82%AC") == "\u20ac"  # UTF-8 euro round-trips

# --- query strings ----------------------------------------------------------
assert parse_qs("a=1&a=2&b=3") == {"a": ["1", "2"], "b": ["3"]}
assert parse_qsl("a=1&a=2&b=3") == [("a", "1"), ("a", "2"), ("b", "3")]
assert urlencode({"a": "1", "b": "x y"}) == "a=1&b=x+y"
assert urlencode([("a", "1"), ("a", "2")]) == "a=1&a=2"
assert urlencode({"a": [1, 2]}, doseq=True) == "a=1&a=2"

print("WS4 urllib.parse fixture ok")
