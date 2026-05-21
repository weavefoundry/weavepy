from urllib.parse import (
    urlparse, urlunparse, urlsplit, urlunsplit,
    urljoin, quote, unquote, urlencode, parse_qsl, parse_qs,
)

u = urlparse("https://user:pass@example.com:8080/path/to?key=val&x=1#frag")
print("scheme:", u.scheme)
print("netloc:", u.netloc)
print("hostname:", u.hostname)
print("port:", u.port)
print("path:", u.path)
print("query:", u.query)
print("fragment:", u.fragment)
print("username:", u.username)
print("password:", u.password)
print("unparse:", urlunparse(u))

s = urlsplit("https://example.com/foo?bar=1#z")
print("split:", s.scheme, s.netloc, s.path, s.query, s.fragment)
print("unsplit:", urlunsplit(s))

print("urljoin:", urljoin("https://a.com/x/y", "../z?q=1"))
print("urljoin abs:", urljoin("https://a.com/x/", "https://b.com/y"))

print("quote:", quote("hello world & friends"))
print("unquote:", unquote("hello%20world%20%26%20friends"))

print("urlencode:", urlencode({"a": "1", "b": "two words"}))
print("parse_qsl:", parse_qsl("a=1&b=two+words&a=2"))
print("parse_qs:", parse_qs("a=1&b=two+words&a=2"))
