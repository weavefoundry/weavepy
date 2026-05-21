"""WeavePy `http.server` — a tiny HTTP/1.1 server.

Provides:
* `HTTPServer(server_address, RequestHandlerClass)` — single-threaded
  blocking server. `serve_forever`, `shutdown`, `server_close`.
* `BaseHTTPRequestHandler` — request parsing + response helpers.
  Subclass and override `do_GET`, `do_POST`, etc.
* `SimpleHTTPRequestHandler` — serves files from `directory`.
"""

import os
import socket as _socket


__all__ = [
    "HTTPServer", "ThreadingHTTPServer",
    "BaseHTTPRequestHandler", "SimpleHTTPRequestHandler",
]


_RESPONSES = {
    200: ("OK", "Request fulfilled, document follows"),
    201: ("Created", "Document created, URL follows"),
    204: ("No Content", "Request fulfilled, nothing follows"),
    301: ("Moved Permanently", "Object moved permanently -- see URI list"),
    302: ("Found", "Object moved temporarily -- see URI list"),
    304: ("Not Modified", "Document has not changed since given time"),
    400: ("Bad Request", "Bad request syntax or unsupported method"),
    401: ("Unauthorized", "No permission -- see authorization schemes"),
    403: ("Forbidden", "Request forbidden -- authorization will not help"),
    404: ("Not Found", "Nothing matches the given URI"),
    405: ("Method Not Allowed", "Specified method is invalid for this resource"),
    500: ("Internal Server Error", "Server got itself in trouble"),
    501: ("Not Implemented", "Server does not support this operation"),
    503: ("Service Unavailable", "The server cannot process the request due to a high load"),
}


class HTTPServer:
    """A simple single-threaded HTTP server."""

    def __init__(self, server_address, RequestHandlerClass):
        self.server_address = server_address
        self.RequestHandlerClass = RequestHandlerClass
        self.socket = _socket.socket(_socket.AF_INET, _socket.SOCK_STREAM)
        try:
            self.socket.setsockopt(_socket.SOL_SOCKET, _socket.SO_REUSEADDR, 1)
        except OSError:
            pass
        self.socket.bind(server_address)
        self.socket.listen(5)
        self._running = False

    def serve_forever(self, poll_interval=0.5):
        self._running = True
        while self._running:
            try:
                conn, addr = self.socket.accept()
            except OSError:
                if not self._running:
                    break
                raise
            try:
                self.RequestHandlerClass(conn, addr, self)
            finally:
                try:
                    conn.close()
                except Exception:
                    pass

    def handle_request(self):
        conn, addr = self.socket.accept()
        try:
            self.RequestHandlerClass(conn, addr, self)
        finally:
            try:
                conn.close()
            except Exception:
                pass

    def shutdown(self):
        self._running = False

    def server_close(self):
        try:
            self.socket.close()
        except Exception:
            pass

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.server_close()
        return False


class ThreadingHTTPServer(HTTPServer):
    """Threaded variant — until cooperative threading is fully wired,
    this falls back to single-threaded behavior."""


class BaseHTTPRequestHandler:
    """Base class for HTTP request handlers."""

    protocol_version = "HTTP/1.0"
    default_request_version = "HTTP/0.9"
    server_version = "WeavePyHTTP/0.1"
    sys_version = "WeavePy"

    def __init__(self, request, client_address, server):
        self.request = request
        self.client_address = client_address
        self.server = server
        self.headers = {}
        self.command = None
        self.path = None
        self.request_version = None
        self.raw_requestline = b""
        self._body = b""
        self._wbuf = []
        try:
            self.handle()
        finally:
            self.finish()

    def setup(self):
        pass

    def handle(self):
        self.handle_one_request()

    def handle_one_request(self):
        raw = self._recv_all()
        head_end = raw.find(b"\r\n\r\n")
        if head_end == -1:
            head = raw.decode("iso-8859-1", "replace")
            body = b""
        else:
            head = raw[:head_end].decode("iso-8859-1", "replace")
            body = raw[head_end + 4:]
        parts = head.split("\r\n")
        if not parts:
            self.send_error(400, "Empty request")
            return
        self.raw_requestline = parts[0].encode("ascii", "replace")
        bits = parts[0].split(" ")
        if len(bits) < 2:
            self.send_error(400, "Bad request line")
            return
        self.command = bits[0]
        self.path = bits[1]
        self.request_version = bits[2] if len(bits) >= 3 else self.default_request_version
        for line in parts[1:]:
            if ":" in line:
                k, _, v = line.partition(":")
                self.headers[k.strip().title()] = v.strip()
        self._body = body
        method = "do_" + self.command
        handler = getattr(self, method, None)
        if handler is None:
            self.send_error(501, "Unsupported method {!r}".format(self.command))
            return
        handler()

    def _recv_all(self):
        chunks = []
        while True:
            try:
                chunk = self.request.recv(4096)
            except Exception:
                break
            if not chunk:
                break
            chunks.append(chunk)
            if b"\r\n\r\n" in b"".join(chunks):
                blob = b"".join(chunks)
                head_end = blob.find(b"\r\n\r\n")
                head = blob[:head_end].decode("iso-8859-1", "replace")
                cl = 0
                for line in head.split("\r\n"):
                    if line.lower().startswith("content-length:"):
                        try:
                            cl = int(line.split(":", 1)[1].strip())
                        except ValueError:
                            cl = 0
                while len(blob) - head_end - 4 < cl:
                    more = self.request.recv(4096)
                    if not more:
                        break
                    blob += more
                return blob
        return b"".join(chunks)

    def send_response(self, code, message=None):
        if message is None:
            message = _RESPONSES.get(code, ("???",))[0]
        self._wbuf.append("HTTP/1.0 {} {}\r\n".format(code, message))
        self.send_header("Server", self.server_version)

    def send_header(self, key, value):
        self._wbuf.append("{}: {}\r\n".format(key, value))

    def end_headers(self):
        self._wbuf.append("\r\n")
        self.request.sendall("".join(self._wbuf).encode("iso-8859-1"))
        self._wbuf = []

    def wfile_write(self, data):
        if isinstance(data, str):
            data = data.encode("utf-8")
        self.request.sendall(data)

    @property
    def wfile(self):
        handler = self

        class _W:
            def write(self, data):
                handler.wfile_write(data)

            def flush(self):
                pass
        return _W()

    @property
    def rfile(self):
        body = self._body
        pos = [0]

        class _R:
            def read(self, amt=-1):
                if amt == -1:
                    data = body[pos[0]:]
                    pos[0] = len(body)
                    return data
                data = body[pos[0]:pos[0] + amt]
                pos[0] += len(data)
                return data

            def readline(self):
                idx = body.find(b"\n", pos[0])
                if idx == -1:
                    data = body[pos[0]:]
                    pos[0] = len(body)
                    return data
                data = body[pos[0]:idx + 1]
                pos[0] = idx + 1
                return data
        return _R()

    def send_error(self, code, message=None, explain=None):
        if message is None:
            message = _RESPONSES.get(code, ("???",))[0]
        body = ("Error {}: {}".format(code, message)).encode("ascii")
        self.send_response(code, message)
        self.send_header("Content-Type", "text/plain; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile_write(body)

    def log_request(self, code="-", size="-"):
        pass

    def log_message(self, fmt, *args):
        pass

    def address_string(self):
        return self.client_address[0]

    def date_time_string(self):
        import time
        return time.strftime("%a, %d %b %Y %H:%M:%S GMT")

    def finish(self):
        pass


class SimpleHTTPRequestHandler(BaseHTTPRequestHandler):
    """Serves files relative to `directory` (default: cwd)."""

    def __init__(self, request, client_address, server, *, directory=None):
        self.directory = directory or os.getcwd()
        BaseHTTPRequestHandler.__init__(self, request, client_address, server)

    def do_GET(self):
        path = self.path.lstrip("/")
        full = os.path.join(self.directory, path)
        if not os.path.exists(full):
            self.send_error(404, "Not found: {}".format(path))
            return
        try:
            with open(full, "rb") as f:
                data = f.read()
        except OSError:
            self.send_error(500, "Failed to read")
            return
        self.send_response(200, "OK")
        self.send_header("Content-Type", "application/octet-stream")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile_write(data)

    def do_HEAD(self):
        path = self.path.lstrip("/")
        full = os.path.join(self.directory, path)
        if not os.path.exists(full):
            self.send_error(404, "Not found: {}".format(path))
            return
        try:
            size = os.path.getsize(full)
        except OSError:
            self.send_error(500, "stat failed")
            return
        self.send_response(200, "OK")
        self.send_header("Content-Type", "application/octet-stream")
        self.send_header("Content-Length", str(size))
        self.end_headers()
