"""WeavePy `urllib.request` — HTTP-only `urlopen` on top of sockets.

Scope:
* `urlopen(url, data=None, timeout=...)` — opens an HTTP URL.
* `Request(url, data=None, headers=None, method=None)` — request
  builder.
* `HTTPError`, `URLError` — exception types.
* `build_opener`, `install_opener`, `OpenerDirector` — opener API
  surface.

Out of scope (raises `URLError` / `NotImplementedError`):
* HTTPS — needs the TLS layer that RFC 0017 defers.
* `file://`, `ftp://`, `data:` — niche; out of scope.
* HTTP Basic Auth handler chain (the constants exist; the wiring
  through cookies / digest / negotiate is a follow-up).
"""

import socket as _socket

from urllib.error import URLError, HTTPError
from urllib.parse import urlparse, urlencode, quote as _quote, unquote as _unquote


def url2pathname(pathname):
    """OS-specific conversion from a relative URL of the 'file' scheme to a
    file system path; not recommended for general use (POSIX form)."""
    return _unquote(pathname)


def pathname2url(pathname):
    """OS-specific conversion from a file system path to a relative URL of the
    'file' scheme; not recommended for general use (POSIX form)."""
    return _quote(pathname)


_DEFAULT_USER_AGENT = "WeavePy-urllib/0.1"


class Request:
    """A pending HTTP request."""

    def __init__(self, url, data=None, headers=None, origin_req_host=None,
                 unverifiable=False, method=None):
        self.full_url = url
        self._data = data
        self.headers = {}
        if headers:
            for k, v in headers.items():
                self.add_header(k, v)
        self._method = method
        self.origin_req_host = origin_req_host
        self.unverifiable = unverifiable

    @property
    def data(self):
        return self._data

    @data.setter
    def data(self, value):
        self._data = value

    def get_method(self):
        if self._method is not None:
            return self._method
        return "POST" if self._data is not None else "GET"

    def add_header(self, key, value):
        self.headers[key.title()] = value

    def has_header(self, header_name):
        return header_name.title() in self.headers

    def get_header(self, header_name, default=None):
        return self.headers.get(header_name.title(), default)


class _HTTPResponse:
    """A minimal file-like HTTP response."""

    def __init__(self, status, reason, headers, body, url):
        self.status = status
        self.reason = reason
        self._headers = headers
        self._body = body
        self._body_pos = 0
        self.url = url
        self.code = status
        self.msg = reason

    def read(self, amt=None):
        if amt is None:
            data = self._body[self._body_pos:]
            self._body_pos = len(self._body)
            return data
        data = self._body[self._body_pos:self._body_pos + amt]
        self._body_pos += len(data)
        return data

    def readline(self):
        idx = self._body.find(b"\n", self._body_pos)
        if idx == -1:
            data = self._body[self._body_pos:]
            self._body_pos = len(self._body)
            return data
        data = self._body[self._body_pos:idx + 1]
        self._body_pos = idx + 1
        return data

    def getheaders(self):
        return list(self._headers.items())

    def getheader(self, name, default=None):
        return self._headers.get(name.title(), default)

    def info(self):
        return _HeadersBag(self._headers)

    def geturl(self):
        return self.url

    def getcode(self):
        return self.status

    def close(self):
        self._body = b""

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.close()
        return False


class _HeadersBag:
    def __init__(self, headers):
        self._headers = headers

    def get(self, name, default=None):
        return self._headers.get(name.title(), default)

    def __getitem__(self, name):
        return self._headers[name.title()]

    def items(self):
        return self._headers.items()

    def __iter__(self):
        return iter(self._headers)


def _build_request_bytes(method, host, path, headers, body):
    lines = ["{} {} HTTP/1.1".format(method, path or "/")]
    seen = set(h.lower() for h in headers)
    if "host" not in seen:
        lines.append("Host: {}".format(host))
    if "user-agent" not in seen:
        lines.append("User-Agent: {}".format(_DEFAULT_USER_AGENT))
    if "connection" not in seen:
        lines.append("Connection: close")
    if body is not None and "content-length" not in seen:
        lines.append("Content-Length: {}".format(len(body)))
    for k, v in headers.items():
        lines.append("{}: {}".format(k, v))
    blob = ("\r\n".join(lines) + "\r\n\r\n").encode("ascii")
    if body is not None:
        if isinstance(body, str):
            body = body.encode("utf-8")
        blob += body
    return blob


def _parse_response(raw):
    # Split status line + headers + body.
    head_end = raw.find(b"\r\n\r\n")
    if head_end == -1:
        head_end = len(raw)
        body = b""
    else:
        body = raw[head_end + 4:]
    head = raw[:head_end].decode("iso-8859-1", errors="replace")
    parts = head.split("\r\n")
    status_line = parts[0]
    bits = status_line.split(" ", 2)
    if len(bits) < 3:
        raise URLError("malformed status line: {!r}".format(status_line))
    try:
        status = int(bits[1])
    except ValueError:
        raise URLError("non-int status: {!r}".format(bits[1]))
    reason = bits[2]
    headers = {}
    for line in parts[1:]:
        if not line or ":" not in line:
            continue
        k, _, v = line.partition(":")
        headers[k.strip().title()] = v.strip()
    return status, reason, headers, body


def urlopen(url, data=None, timeout=None):
    """Open `url`. HTTP via plain sockets, HTTPS via the Rust ``_https``
    accelerator (which wraps rustls).
    """
    if isinstance(url, Request):
        req = url
    else:
        req = Request(url, data=data)
    parts = urlparse(req.full_url)
    if parts.scheme not in ("http", "https"):
        raise URLError("unsupported scheme: {!r}".format(parts.scheme))
    host = parts.hostname
    if host is None:
        raise URLError("URL has no host: {!r}".format(req.full_url))
    body = None
    if req.data is not None:
        body = req.data
        if isinstance(body, dict):
            body = urlencode(body).encode("ascii")
    path = parts.path or "/"
    if parts.query:
        path = path + "?" + parts.query
    method = req.get_method()
    if parts.scheme == "https":
        try:
            import _https
        except ImportError as e:
            raise URLError("HTTPS unavailable: {}".format(e))
        port = parts.port or 443
        headers = dict(req.headers)
        headers.setdefault("User-Agent", "WeavePy-urllib/1.0")
        status, hdrs, body_bytes = _https.request(method, host, port, path, headers, body or b"")
        reason = ""
        return _HTTPResponse(status, reason, hdrs, body_bytes, req.full_url)
    port = parts.port or 80
    blob = _build_request_bytes(method, host, path, req.headers, body)
    sock = _socket.socket(_socket.AF_INET, _socket.SOCK_STREAM)
    try:
        if timeout is not None:
            sock.settimeout(timeout)
        sock.connect((host, port))
        sock.sendall(blob)
        chunks = []
        while True:
            chunk = sock.recv(8192)
            if not chunk:
                break
            chunks.append(chunk)
        raw = b"".join(chunks)
    finally:
        try:
            sock.close()
        except Exception:
            pass
    status, reason, headers, body = _parse_response(raw)
    if status >= 400:
        raise HTTPError(req.full_url, status, reason, headers, None)
    return _HTTPResponse(status, reason, headers, body, req.full_url)


class OpenerDirector:
    """A pluggable opener that dispatches through handler chains."""

    def __init__(self):
        self.handlers = []

    def add_handler(self, handler):
        self.handlers.append(handler)

    def open(self, fullurl, data=None, timeout=None):
        return urlopen(fullurl, data, timeout)


def build_opener(*handlers):
    opener = OpenerDirector()
    for h in handlers:
        opener.add_handler(h)
    return opener


_installed_opener = None


def install_opener(opener):
    global _installed_opener
    _installed_opener = opener


def getproxies():
    return {}


class BaseHandler:
    handler_order = 500


class HTTPHandler(BaseHandler):
    def http_open(self, req):
        return urlopen(req)


class HTTPSHandler(BaseHandler):
    def https_open(self, req):
        return urlopen(req)


class HTTPRedirectHandler(BaseHandler):
    pass


class HTTPBasicAuthHandler(BaseHandler):
    pass


class HTTPDigestAuthHandler(BaseHandler):
    pass


class HTTPDefaultErrorHandler(BaseHandler):
    pass


def urlretrieve(url, filename=None, reporthook=None, data=None):
    resp = urlopen(url, data=data)
    body = resp.read()
    if filename is None:
        import tempfile
        fd, filename = tempfile.mkstemp(suffix=".urlretrieve")
    with open(filename, "wb") as f:
        f.write(body)
    return (filename, _HeadersBag(resp._headers))


__all__ = [
    "Request", "urlopen", "urlretrieve", "OpenerDirector",
    "build_opener", "install_opener", "BaseHandler", "HTTPHandler",
    "HTTPSHandler", "HTTPRedirectHandler", "HTTPBasicAuthHandler",
    "HTTPDigestAuthHandler", "HTTPDefaultErrorHandler", "getproxies",
    "pathname2url", "url2pathname",
]
