"""WeavePy `http.client` — small HTTP/1.1 client.

Provides:
* `HTTPConnection(host, port=80, timeout=...)` — basic plain-text
  HTTP. `request`, `getresponse`, `close`, `set_debuglevel`.
* `HTTPSConnection` — class exists; raises on connect because the
  TLS engine isn't shipped yet (see RFC 0017).
* `HTTPResponse` — file-like with `read`, `readline`, `status`,
  `reason`, `getheader`, `getheaders`, `info`, `geturl`.
* `HTTPException` and the standard error subclasses.

The full surface (chunked transfer encoding, pipelining,
`HTTPConnection.send` for streaming bodies) is *partially* covered:
chunked decoding is supported on response read; outbound chunked
encoding is not.
"""

import socket as _socket


# Status code constants — a representative subset.
OK = 200
CREATED = 201
ACCEPTED = 202
NO_CONTENT = 204
MOVED_PERMANENTLY = 301
FOUND = 302
SEE_OTHER = 303
NOT_MODIFIED = 304
TEMPORARY_REDIRECT = 307
BAD_REQUEST = 400
UNAUTHORIZED = 401
FORBIDDEN = 403
NOT_FOUND = 404
METHOD_NOT_ALLOWED = 405
INTERNAL_SERVER_ERROR = 500
BAD_GATEWAY = 502
SERVICE_UNAVAILABLE = 503

HTTP_PORT = 80
HTTPS_PORT = 443

responses = {
    200: "OK",
    201: "Created",
    202: "Accepted",
    204: "No Content",
    301: "Moved Permanently",
    302: "Found",
    303: "See Other",
    304: "Not Modified",
    307: "Temporary Redirect",
    400: "Bad Request",
    401: "Unauthorized",
    403: "Forbidden",
    404: "Not Found",
    405: "Method Not Allowed",
    500: "Internal Server Error",
    502: "Bad Gateway",
    503: "Service Unavailable",
}


class HTTPException(Exception):
    pass


class NotConnected(HTTPException):
    pass


class InvalidURL(HTTPException):
    pass


class UnknownProtocol(HTTPException):
    pass


class IncompleteRead(HTTPException):
    def __init__(self, partial, expected=None):
        self.partial = partial
        self.expected = expected
        self.args = (partial, expected)


class HTTPResponse:
    """Response object returned by `HTTPConnection.getresponse()`."""

    def __init__(self, status, reason, headers, body, url):
        self.status = status
        self.reason = reason
        self._headers = headers
        self._body = body
        self._pos = 0
        self.url = url
        self.code = status
        self.msg = reason
        self.version = 11

    def read(self, amt=None):
        if amt is None:
            data = self._body[self._pos:]
            self._pos = len(self._body)
            return data
        data = self._body[self._pos:self._pos + amt]
        self._pos += len(data)
        return data

    def readline(self):
        idx = self._body.find(b"\n", self._pos)
        if idx == -1:
            data = self._body[self._pos:]
            self._pos = len(self._body)
            return data
        data = self._body[self._pos:idx + 1]
        self._pos = idx + 1
        return data

    def getheader(self, name, default=None):
        return self._headers.get(name.title(), default)

    def getheaders(self):
        return list(self._headers.items())

    def info(self):
        class _Bag:
            def __init__(self, h):
                self._h = h

            def get(self, k, default=None):
                return self._h.get(k.title(), default)

            def __getitem__(self, k):
                return self._h[k.title()]

            def items(self):
                return self._h.items()
        return _Bag(self._headers)

    def close(self):
        self._body = b""

    def isclosed(self):
        return self._pos >= len(self._body)

    def fileno(self):
        raise OSError("HTTPResponse has no underlying fd")


def _read_chunked(raw):
    # Decode a chunked-transfer-encoding body.
    out = bytearray()
    pos = 0
    while pos < len(raw):
        nl = raw.find(b"\r\n", pos)
        if nl == -1:
            break
        size_line = raw[pos:nl]
        try:
            size = int(size_line.split(b";", 1)[0].strip(), 16)
        except ValueError:
            break
        pos = nl + 2
        if size == 0:
            break
        out.extend(raw[pos:pos + size])
        pos += size + 2  # skip trailing \r\n.
    return bytes(out)


class HTTPConnection:
    """A pending connection to a single HTTP server."""

    default_port = HTTP_PORT

    def __init__(self, host, port=None, timeout=None):
        if port is None:
            if ":" in host and not host.startswith("["):
                host, port_str = host.rsplit(":", 1)
                port = int(port_str)
            else:
                port = self.default_port
        self.host = host
        self.port = port
        self.timeout = timeout
        self._sock = None
        self._request_buffer = []
        self._method = None

    def connect(self):
        sock = _socket.socket(_socket.AF_INET, _socket.SOCK_STREAM)
        if self.timeout is not None:
            sock.settimeout(self.timeout)
        sock.connect((self.host, self.port))
        self._sock = sock

    def close(self):
        if self._sock is not None:
            try:
                self._sock.close()
            except Exception:
                pass
            self._sock = None

    def putrequest(self, method, url, skip_host=False, skip_accept_encoding=False):
        self._method = method
        self._request_buffer = ["{} {} HTTP/1.1".format(method, url or "/")]
        if not skip_host:
            self._request_buffer.append("Host: {}".format(self.host))
        if not skip_accept_encoding:
            self._request_buffer.append("Accept-Encoding: identity")

    def putheader(self, key, value):
        self._request_buffer.append("{}: {}".format(key, value))

    def endheaders(self, message_body=None, encode_chunked=False):
        if self._sock is None:
            self.connect()
        blob = ("\r\n".join(self._request_buffer) + "\r\n\r\n").encode("ascii")
        if message_body is not None:
            if isinstance(message_body, str):
                message_body = message_body.encode("utf-8")
            blob += message_body
        self._sock.sendall(blob)
        self._request_buffer = []

    def request(self, method, url, body=None, headers=None, *, encode_chunked=False):
        self.putrequest(method, url)
        if headers:
            for k, v in headers.items():
                self.putheader(k, v)
        if body is not None and not (headers and any(k.lower() == "content-length" for k in headers)):
            self.putheader("Content-Length", str(len(body) if isinstance(body, (bytes, bytearray)) else len(body.encode("utf-8"))))
        if not headers or not any(k.lower() == "connection" for k in headers):
            self.putheader("Connection", "close")
        self.endheaders(body)

    def getresponse(self):
        if self._sock is None:
            raise NotConnected("HTTPConnection is not connected")
        chunks = []
        while True:
            chunk = self._sock.recv(8192)
            if not chunk:
                break
            chunks.append(chunk)
        raw = b"".join(chunks)
        head_end = raw.find(b"\r\n\r\n")
        if head_end == -1:
            head = raw.decode("iso-8859-1", "replace")
            body = b""
        else:
            head = raw[:head_end].decode("iso-8859-1", "replace")
            body = raw[head_end + 4:]
        parts = head.split("\r\n")
        bits = parts[0].split(" ", 2)
        if len(bits) < 3:
            raise HTTPException("malformed status line: {!r}".format(parts[0]))
        try:
            status = int(bits[1])
        except ValueError:
            raise HTTPException("non-int status: {!r}".format(bits[1]))
        reason = bits[2]
        headers = {}
        chunked = False
        for line in parts[1:]:
            if ":" in line:
                k, _, v = line.partition(":")
                headers[k.strip().title()] = v.strip()
                if k.strip().lower() == "transfer-encoding" and "chunked" in v.lower():
                    chunked = True
        if chunked:
            body = _read_chunked(body)
        return HTTPResponse(status, reason, headers, body, self.host)

    def set_debuglevel(self, level):
        pass

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.close()
        return False


class HTTPSConnection(HTTPConnection):
    """HTTPS connection — requires TLS, not yet supported."""

    default_port = HTTPS_PORT

    def __init__(self, host, port=None, key_file=None, cert_file=None,
                 timeout=None, context=None, check_hostname=None):
        HTTPConnection.__init__(self, host, port, timeout)
        self._context = context

    def connect(self):
        raise HTTPException("HTTPS support requires the TLS engine (RFC 0017 future work)")


__all__ = [
    "HTTPConnection", "HTTPSConnection", "HTTPResponse", "HTTPException",
    "NotConnected", "InvalidURL", "UnknownProtocol", "IncompleteRead",
    "responses", "HTTP_PORT", "HTTPS_PORT",
    "OK", "CREATED", "ACCEPTED", "NO_CONTENT",
    "MOVED_PERMANENTLY", "FOUND", "SEE_OTHER", "NOT_MODIFIED",
    "TEMPORARY_REDIRECT", "BAD_REQUEST", "UNAUTHORIZED", "FORBIDDEN",
    "NOT_FOUND", "METHOD_NOT_ALLOWED", "INTERNAL_SERVER_ERROR",
    "BAD_GATEWAY", "SERVICE_UNAVAILABLE",
]
