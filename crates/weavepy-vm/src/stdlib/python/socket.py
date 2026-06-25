"""WeavePy `socket` — convenience layer over the Rust `_socket` core.

The Rust `_socket` module already exposes the constants, the
`socket.socket` class, and the module-level resolution helpers
(`getaddrinfo`, `gethostname`, etc.). This file re-exports them and
adds the tiny pure-Python convenience helpers CPython ships on top:

* `socket.timeout` alias (matches CPython 3.10+ behaviour where it
  is `TimeoutError`).
* `socket.create_connection(address, timeout=DEFAULT, source_address=None)` —
  multi-address resolution retrying. The Rust core already provides a
  single-shot version; this wrapper retries through `getaddrinfo`.
* A `_GLOBAL_DEFAULT_TIMEOUT` sentinel for the timeout-default
  protocol.

`socket.socket.makefile()` from the Rust side returns a minimal
file-like dict (`read`/`write`/`close`). Real `io.BufferedReader`
ergonomics on top are out of scope for this RFC.
"""

import _socket as _impl

# Re-export everything from the Rust core. We do this dynamically so
# new constants added on the Rust side appear in `socket` without
# touching this file.
for _name in dir(_impl):
    if _name.startswith("__"):
        continue
    globals()[_name] = getattr(_impl, _name)


_GLOBAL_DEFAULT_TIMEOUT = object()


# `socket.timeout` is aliased to `TimeoutError` since CPython 3.10.
timeout = TimeoutError


def create_connection(address, timeout=_GLOBAL_DEFAULT_TIMEOUT, source_address=None):
    """Open a TCP connection.

    Walks `getaddrinfo`, attempting each candidate in turn, and
    returns the first successfully-connected socket. Matches CPython.
    """
    host, port = address
    err = None
    for res in _impl.getaddrinfo(host, port, 0, _impl.SOCK_STREAM):
        af, socktype, proto, _cn, sa = res
        sock = None
        try:
            sock = _impl.socket(af, socktype, proto)
            if timeout is not _GLOBAL_DEFAULT_TIMEOUT and timeout is not None:
                sock.settimeout(timeout)
            if source_address is not None:
                sock.bind(source_address)
            sock.connect(sa)
            return sock
        except OSError as exc:
            err = exc
            if sock is not None:
                try:
                    sock.close()
                except Exception:
                    pass
    if err is not None:
        raise err
    raise OSError("getaddrinfo returned an empty list")


def create_server(address, *, family=None, backlog=None, reuse_port=False, dualstack_ipv6=False):
    """Convenience to bind a listening TCP socket. Mirrors CPython."""
    fam = family if family is not None else _impl.AF_INET
    return _impl.create_server(address, fam, backlog if backlog is not None else 100, reuse_port)


def fromfd(fd, family, type, proto=0):
    """Create a socket object from a *duplicate* of the given file
    descriptor (CPython parity). The remaining arguments are the same as
    for `socket()`. `multiprocessing.reduction.send_handle`/`recv_handle`
    wrap a Connection's fd this way to push file descriptors over
    `SCM_RIGHTS`.
    """
    import os
    nfd = os.dup(fd)
    return _impl.socket(family, type, proto, nfd)


def getfqdn(name=""):
    return _impl.getfqdn(name)


def has_ipv6():
    return True


__all__ = [
    "socket", "AF_INET", "AF_INET6", "AF_UNIX", "SOCK_STREAM",
    "SOCK_DGRAM", "SOL_SOCKET", "SO_REUSEADDR", "SO_REUSEPORT",
    "SO_BROADCAST", "SO_KEEPALIVE", "SO_LINGER", "SO_SNDBUF",
    "SO_RCVBUF", "TCP_NODELAY", "IPPROTO_TCP", "IPPROTO_UDP",
    "IPPROTO_IP", "IPPROTO_IPV6", "MSG_OOB", "MSG_PEEK", "MSG_WAITALL",
    "MSG_DONTWAIT", "SHUT_RD", "SHUT_WR", "SHUT_RDWR", "AI_PASSIVE",
    "AI_CANONNAME", "AI_NUMERICHOST", "AI_NUMERICSERV",
    "NI_NUMERICHOST", "NI_NUMERICSERV", "NI_NAMEREQD", "NI_DGRAM",
    "INADDR_ANY", "INADDR_LOOPBACK", "INADDR_BROADCAST",
    "gethostname", "gethostbyname", "gethostbyaddr", "getaddrinfo",
    "getnameinfo", "create_connection", "create_server", "socketpair",
    "inet_aton", "inet_ntoa", "inet_pton", "inet_ntop", "htons",
    "htonl", "ntohs", "ntohl", "getdefaulttimeout", "setdefaulttimeout",
    "error", "herror", "gaierror", "timeout", "has_ipv6", "getfqdn",
    "SocketType",
]
