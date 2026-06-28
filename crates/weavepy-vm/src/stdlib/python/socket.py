"""WeavePy `socket` — CPython-faithful layer over the Rust `_socket` core.

The Rust `_socket` module exposes the constants, a `_socket.socket`
base type (real `socket2`/libc fds), and the module-level resolution
helpers (`getaddrinfo`, `gethostname`, …). This module mirrors
CPython's `Lib/socket.py`: it defines `socket(_socket.socket)` — the
public socket type that adds `makefile()` returning a genuine buffered
`io` stream via `SocketIO(io.RawIOBase)` — so the verbatim
`http.client`/`ftplib`/`smtplib`/`imaplib`/`poplib` drivers (all of
which do `sock.makefile("rb")`) work unchanged.
"""

import _socket as _impl
import io
import os
import errno as _errno

# Re-export everything from the Rust core. We do this dynamically so
# new constants added on the Rust side appear in `socket` without
# touching this file. The public `socket` type and the few helpers that
# must return it are (re)defined below, overriding the raw re-exports.
for _name in dir(_impl):
    if _name.startswith("__"):
        continue
    globals()[_name] = getattr(_impl, _name)


_GLOBAL_DEFAULT_TIMEOUT = object()

# `socket.timeout` is aliased to `TimeoutError` since CPython 3.10.
timeout = TimeoutError

# errnos that signal "would block" on a non-blocking socket; SocketIO
# turns these into a `None` return (CPython parity).
_blocking_errnos = {_errno.EAGAIN, _errno.EWOULDBLOCK}


class socket(_impl.socket):
    """A subclass of _socket.socket adding the makefile() method."""

    def __init__(self, family=-1, type=-1, proto=-1, fileno=None):
        if fileno is None:
            if family == -1:
                family = AF_INET
            if type == -1:
                type = SOCK_STREAM
            if proto == -1:
                proto = 0
        _impl.socket.__init__(self, family, type, proto, fileno)
        self._io_refs = 0
        self._closed = False
        # CPython stamps every *freshly created* socket with the module-wide
        # default timeout (``socket.setdefaulttimeout``); sockets adopted from
        # an existing ``fileno`` instead inherit that fd's blocking state. Mirror
        # that here so e.g. ``http.client`` picks up a global default timeout
        # (``test_httplib.testTimeoutAttribute``).
        if fileno is None:
            _default = getdefaulttimeout()
            if _default is not None:
                self.settimeout(_default)

    def __enter__(self):
        return self

    def __exit__(self, *args):
        if not self._closed:
            self.close()

    def __repr__(self):
        closed = getattr(self, "_closed", False)
        try:
            fd = self.fileno()
        except Exception:
            fd = -1
        s = "<socket.socket%s fd=%i, family=%s, type=%s, proto=%i" % (
            " [closed]" if closed else "",
            fd,
            self.family,
            self.type,
            self.proto,
        )
        if not closed:
            try:
                laddr = self.getsockname()
                if laddr:
                    s += ", laddr=%s" % str(laddr)
            except (error, AttributeError):
                pass
            try:
                raddr = self.getpeername()
                if raddr:
                    s += ", raddr=%s" % str(raddr)
            except (error, AttributeError):
                pass
        s += ">"
        return s

    def __getstate__(self):
        raise TypeError(f"cannot pickle {self.__class__.__name__!r} object")

    def dup(self):
        """dup() -> socket object

        Duplicate the socket. Return a new socket object connected to the
        same system resource. The new socket is non-inheritable.
        """
        fd = os.dup(self.fileno())
        sock = self.__class__(self.family, self.type, self.proto, fileno=fd)
        sock.settimeout(self.gettimeout())
        return sock

    def accept(self):
        """accept() -> (socket object, address info)

        Wait for an incoming connection.  Return a new socket
        representing the connection, and the address of the client.
        """
        raw, addr = _impl.socket.accept(self)
        # The Rust core hands back a bare `_socket.socket`; adopt its fd into
        # a public `socket` (so the accepted connection also has makefile()).
        fd = raw.detach()
        sock = socket(self.family, self.type, self.proto, fileno=fd)
        if getdefaulttimeout() is None and self.gettimeout():
            sock.setblocking(True)
        return sock, addr

    def makefile(self, mode="r", buffering=None, *,
                 encoding=None, errors=None, newline=None):
        """makefile(...) -> an I/O stream connected to the socket

        The arguments are as for io.open() after the filename, except the
        only supported mode values are 'r' (default), 'w', 'b', or a
        combination of those.
        """
        if not set(mode) <= {"r", "w", "b"}:
            raise ValueError("invalid mode %r (only r, w, b allowed)" % (mode,))
        writing = "w" in mode
        reading = "r" in mode or not writing
        assert reading or writing
        binary = "b" in mode
        rawmode = ""
        if reading:
            rawmode += "r"
        if writing:
            rawmode += "w"
        raw = SocketIO(self, rawmode)
        self._io_refs += 1
        if buffering is None:
            buffering = -1
        if buffering < 0:
            buffering = io.DEFAULT_BUFFER_SIZE
        if buffering == 0:
            if not binary:
                raise ValueError("unbuffered streams must be binary")
            return raw
        if reading and writing:
            buffer = io.BufferedRWPair(raw, raw, buffering)
        elif reading:
            buffer = io.BufferedReader(raw, buffering)
        else:
            assert writing
            buffer = io.BufferedWriter(raw, buffering)
        if binary:
            return buffer
        encoding = io.text_encoding(encoding)
        text = io.TextIOWrapper(buffer, encoding, errors, newline)
        text.mode = mode
        return text

    def _sendfile_use_send(self, file, offset=0, count=None):
        self._check_sendfile_params(file, offset, count)
        if self.gettimeout() == 0:
            raise ValueError("non-blocking sockets are not supported")
        if offset:
            file.seek(offset)
        blocksize = min(count, 8192) if count else 8192
        total_sent = 0
        file_read = file.read
        sock_send = self.send
        try:
            while True:
                if count:
                    blocksize = min(count - total_sent, blocksize)
                    if blocksize <= 0:
                        break
                data = memoryview(file_read(blocksize))
                if not data:
                    break  # EOF
                while True:
                    try:
                        sent = sock_send(data)
                    except BlockingIOError:
                        continue
                    else:
                        total_sent += sent
                        if sent < len(data):
                            data = data[sent:]
                        else:
                            break
            return total_sent
        finally:
            if total_sent > 0 and hasattr(file, "seek"):
                file.seek(offset + total_sent)

    def _check_sendfile_params(self, file, offset, count):
        if "b" not in getattr(file, "mode", "b"):
            raise ValueError("file should be opened in binary mode")
        if not self.type & SOCK_STREAM:
            raise ValueError("only SOCK_STREAM type sockets are supported")
        if count is not None:
            if not isinstance(count, int):
                raise TypeError(
                    "count must be a positive integer (got {!r})".format(count))
            if count <= 0:
                raise ValueError(
                    "count must be a positive integer (got {!r})".format(count))

    def sendfile(self, file, offset=0, count=None):
        """sendfile(file[, offset[, count]]) -> sent

        Send a file until EOF is reached by using send() and return the
        total number of bytes which were sent.
        """
        return self._sendfile_use_send(file, offset, count)

    def _decref_socketios(self):
        if self._io_refs > 0:
            self._io_refs -= 1
        if self._closed:
            self.close()

    def _real_close(self, _ss=_impl.socket):
        _ss.close(self)

    def close(self):
        self._closed = True
        if self._io_refs <= 0:
            self._real_close()

    def detach(self):
        """detach() -> file descriptor

        Close the socket object without closing the underlying file
        descriptor.  The object cannot be used after this call, but the
        file descriptor can be reused for other purposes.  The file
        descriptor is returned.
        """
        self._closed = True
        return _impl.socket.detach(self)


SocketType = socket


class SocketIO(io.RawIOBase):
    """Raw I/O implementation for stream sockets.

    This class supports the makefile() method on sockets.  It provides
    the raw I/O interface on top of a socket object.
    """

    def __init__(self, sock, mode):
        if mode not in ("r", "w", "rw", "rb", "wb", "rwb"):
            raise ValueError("invalid mode: %r" % mode)
        io.RawIOBase.__init__(self)
        self._sock = sock
        if "b" not in mode:
            mode += "b"
        self._mode = mode
        self._reading = "r" in mode
        self._writing = "w" in mode
        self._timeout_occurred = False

    def readinto(self, b):
        """Read up to len(b) bytes into the writable buffer *b* and return
        the number of bytes read.  If the socket is non-blocking and no
        bytes are available, None is returned.

        If *b* is non-empty, a 0 return value indicates that the
        connection was shutdown at the other end.
        """
        self._checkClosed()
        self._checkReadable()
        if self._timeout_occurred:
            raise OSError("cannot read from timed out object")
        try:
            return self._sock.recv_into(b)
        except timeout:
            self._timeout_occurred = True
            raise
        except error as e:
            if e.errno in _blocking_errnos:
                return None
            raise

    def write(self, b):
        """Write the given bytes or bytearray object *b* to the socket
        and return the number of bytes written.  This can be less than
        len(b) if not all data could be written.  If the socket is
        non-blocking and no bytes could be written None is returned.
        """
        self._checkClosed()
        self._checkWritable()
        try:
            return self._sock.send(b)
        except error as e:
            if e.errno in _blocking_errnos:
                return None
            raise

    def readable(self):
        """True if the SocketIO is open for reading."""
        if self.closed:
            raise ValueError("I/O operation on closed socket.")
        return self._reading

    def writable(self):
        """True if the SocketIO is open for writing."""
        if self.closed:
            raise ValueError("I/O operation on closed socket.")
        return self._writing

    def seekable(self):
        """True if the SocketIO is open for seeking."""
        if self.closed:
            raise ValueError("I/O operation on closed socket.")
        return super().seekable()

    def fileno(self):
        """Return the file descriptor of the underlying socket."""
        self._checkClosed()
        return self._sock.fileno()

    @property
    def name(self):
        if not self.closed:
            return self.fileno()
        else:
            return -1

    @property
    def mode(self):
        return self._mode

    def close(self):
        """Close the SocketIO object.  This doesn't close the underlying
        socket, except if all references to it have disappeared.
        """
        if self.closed:
            return
        io.RawIOBase.close(self)
        self._sock._decref_socketios()
        self._sock = None


def create_connection(address, timeout=_GLOBAL_DEFAULT_TIMEOUT, source_address=None,
                      *, all_errors=False):
    """Open a TCP connection.

    Walks `getaddrinfo`, attempting each candidate in turn, and returns
    the first successfully-connected socket. Matches CPython.
    """
    host, port = address
    exceptions = []
    for res in getaddrinfo(host, port, 0, SOCK_STREAM):
        af, socktype, proto, _cn, sa = res
        sock = None
        try:
            sock = socket(af, socktype, proto)
            # CPython only leaves the freshly-constructed socket's timeout
            # (the module default) in place for the `_GLOBAL_DEFAULT_TIMEOUT`
            # sentinel; an explicit ``timeout`` — *including ``None``* — is
            # applied verbatim, so ``HTTPConnection(timeout=None)`` yields a
            # blocking socket even under a non-None default
            # (``test_httplib.testTimeoutAttribute``).
            if timeout is not _GLOBAL_DEFAULT_TIMEOUT:
                sock.settimeout(timeout)
            if source_address is not None:
                sock.bind(source_address)
            sock.connect(sa)
            exceptions.clear()
            return sock
        except error as exc:
            if not all_errors:
                exceptions.clear()
            exceptions.append(exc)
            if sock is not None:
                try:
                    sock.close()
                except Exception:
                    pass
    if len(exceptions):
        try:
            if not all_errors:
                raise exceptions[0]
            raise ExceptionGroup("create_connection failed", exceptions)
        finally:
            exceptions.clear()
    else:
        raise error("getaddrinfo returns an empty list")


def create_server(address, *, family=AF_INET, backlog=None, reuse_port=False,
                  dualstack_ipv6=False):
    """Convenience to bind a listening TCP socket. Mirrors CPython, but
    delegates the bind/listen/SO_REUSEADDR handling to the Rust core and
    re-adopts the resulting fd into a public `socket`."""
    raw = _impl.create_server(address, family, 100 if backlog is None else backlog,
                              reuse_port)
    fd = raw.detach()
    return socket(family, SOCK_STREAM, 0, fileno=fd)


def socketpair(family=None, type=SOCK_STREAM, proto=0):
    """socketpair([family[, type[, proto]]]) -> (socket, socket)

    Create a pair of connected socket objects.
    """
    if family is None:
        family = getattr(_impl, "AF_UNIX", AF_INET)
    a, b = _impl.socketpair(family, type, proto)
    fa, fb = a.detach(), b.detach()
    sa = socket(family, type, proto, fileno=fa)
    sb = socket(family, type, proto, fileno=fb)
    return sa, sb


def fromfd(fd, family, type, proto=0):
    """Create a socket object from a *duplicate* of the given file
    descriptor (CPython parity)."""
    nfd = os.dup(fd)
    return socket(family, type, proto, fileno=nfd)


def getfqdn(name=""):
    return _impl.getfqdn(name)


def has_ipv6():
    return True


def has_dualstack_ipv6():
    """True if the platform supports an AF_INET6 socket able to accept both
    IPv4 and IPv6 connections (CPython parity)."""
    if not hasattr(_impl, "IPPROTO_IPV6") or not hasattr(_impl, "IPV6_V6ONLY"):
        return False
    try:
        with socket(AF_INET6, SOCK_STREAM) as sock:
            sock.setsockopt(IPPROTO_IPV6, IPV6_V6ONLY, 0)
            return True
    except error:
        return False


__all__ = [
    "socket", "SocketIO", "AF_INET", "AF_INET6", "AF_UNIX", "SOCK_STREAM",
    "SOCK_DGRAM", "SOL_SOCKET", "SO_REUSEADDR", "SO_REUSEPORT",
    "SO_BROADCAST", "SO_KEEPALIVE", "SO_LINGER", "SO_SNDBUF",
    "SO_RCVBUF", "TCP_NODELAY", "IPPROTO_TCP", "IPPROTO_UDP",
    "IPPROTO_IP", "IPPROTO_IPV6", "MSG_OOB", "MSG_PEEK", "MSG_WAITALL",
    "MSG_DONTWAIT", "SHUT_RD", "SHUT_WR", "SHUT_RDWR", "AI_PASSIVE",
    "AI_CANONNAME", "AI_NUMERICHOST", "AI_NUMERICSERV",
    "NI_NUMERICHOST", "NI_NUMERICSERV", "NI_NAMEREQD", "NI_DGRAM",
    "INADDR_ANY", "INADDR_LOOPBACK", "INADDR_BROADCAST",
    "gethostname", "gethostbyname", "gethostbyname_ex", "gethostbyaddr", "getaddrinfo",
    "getnameinfo", "create_connection", "create_server", "socketpair",
    "inet_aton", "inet_ntoa", "inet_pton", "inet_ntop", "htons",
    "htonl", "ntohs", "ntohl", "getdefaulttimeout", "setdefaulttimeout",
    "error", "herror", "gaierror", "timeout", "has_ipv6", "getfqdn",
    "fromfd", "SocketType",
]
