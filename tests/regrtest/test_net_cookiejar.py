"""RFC 0042 WS4 — `http.cookiejar` in-process fixture.

Exercises the verbatim-ported `http.cookiejar`: extracting `Set-Cookie`
headers from a faux response into a `CookieJar`, applying them back onto an
outgoing `urllib.request.Request`, and the `MozillaCookieJar` save/load
round-trip. Pure, no network.
"""

import tempfile
import os
from http.cookiejar import CookieJar, MozillaCookieJar
from urllib.request import Request


class FakeResponse:
    """Minimal object implementing the .info() interface cookiejar needs."""

    def __init__(self, headers):
        # headers: list of "Set-Cookie: ..." lines
        from email.message import Message
        self._msg = Message()
        for line in headers:
            name, _, value = line.partition(": ")
            self._msg[name] = value

    def info(self):
        return self._msg


# --- extract cookies from a response, set them on a request ------------------
jar = CookieJar()
req = Request("http://example.com/")
resp = FakeResponse([
    "Set-Cookie: a=1; Path=/",
    "Set-Cookie: b=2; Path=/; Domain=example.com",
])
jar.extract_cookies(resp, req)
names = sorted(c.name for c in jar)
assert names == ["a", "b"], names

out = Request("http://example.com/page")
jar.add_cookie_header(out)
cookie_hdr = out.get_header("Cookie")
assert cookie_hdr is not None
assert "a=1" in cookie_hdr and "b=2" in cookie_hdr, cookie_hdr

# A request to a different domain must not receive example.com cookies.
other = Request("http://other.test/")
jar.add_cookie_header(other)
assert other.get_header("Cookie") is None

# --- MozillaCookieJar save/load round-trip -----------------------------------
path = tempfile.mktemp(suffix=".txt")
try:
    mjar = MozillaCookieJar(path)
    mresp = FakeResponse(["Set-Cookie: persistent=yes; Path=/; Domain=.example.com; expires=Wed, 09 Jun 2099 10:18:14 GMT"])
    mreq = Request("http://www.example.com/")
    mjar.extract_cookies(mresp, mreq)
    mjar.save()

    loaded = MozillaCookieJar(path)
    loaded.load()
    loaded_names = [c.name for c in loaded]
    assert "persistent" in loaded_names, loaded_names
finally:
    if os.path.exists(path):
        os.unlink(path)

print("WS4 http.cookiejar fixture ok")
