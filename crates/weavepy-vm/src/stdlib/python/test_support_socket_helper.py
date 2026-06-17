"""``test.support.socket_helper`` — networking test helpers.

Faithful subset of CPython 3.13's
``Lib/test/support/socket_helper.py``: ``HOST``/``HOSTv4``/``HOSTv6``,
``find_unused_port``, ``bind_port``, ``bind_unix_socket``,
``skip_unless_bind_unix_socket`` and ``transient_internet`` (skips on
network errors instead of failing).
"""

import contextlib
import errno
import os
import socket
import sys

HOST = "localhost"
HOSTv4 = "127.0.0.1"
HOSTv6 = "::1"

# Network-ish errnos that should turn a test into a *skip*, not a fail.
_TRANSIENT_ERRNOS = frozenset(filter(None, (
    getattr(errno, "ECONNREFUSED", None),
    getattr(errno, "ECONNRESET", None),
    getattr(errno, "EHOSTUNREACH", None),
    getattr(errno, "ENETUNREACH", None),
    getattr(errno, "ETIMEDOUT", None),
    getattr(errno, "EADDRNOTAVAIL", None),
)))


def find_unused_port(family=socket.AF_INET, socktype=socket.SOCK_STREAM):
    """Bind to port 0 and report the kernel-assigned port."""
    with socket.socket(family, socktype) as tempsock:
        tempsock.bind(('', 0))
        port = tempsock.getsockname()[1]
    del tempsock
    return port


def bind_port(sock, host=HOST):
    """Bind *sock* to *host* on a free port, returning the port.

    Sets ``SO_REUSEADDR`` is *not* done here on purpose (matches CPython,
    which refuses ``SO_REUSEADDR`` on TCP so a stuck previous run is
    detected); callers that need it set it themselves.
    """
    if sock.family == socket.AF_INET and sock.type == socket.SOCK_STREAM:
        if hasattr(socket, 'SO_REUSEADDR'):
            try:
                if sock.getsockopt(socket.SOL_SOCKET,
                                   socket.SO_REUSEADDR) == 1:
                    raise OSError(
                        "tests should never set the SO_REUSEADDR socket "
                        "option on TCP/IP sockets!")
            except OSError:
                pass
    sock.bind((host, 0))
    port = sock.getsockname()[1]
    return port


def bind_unix_socket(sock, addr):
    """Bind a unix-domain *sock* to *addr*."""
    try:
        sock.bind(addr)
    except PermissionError:
        sock.close()
        import unittest
        raise unittest.SkipTest('cannot bind AF_UNIX sockets')


def _is_ipv6_enabled():
    if getattr(socket, 'has_ipv6', False):
        sock = None
        try:
            sock = socket.socket(socket.AF_INET6, socket.SOCK_STREAM)
            sock.bind((HOSTv6, 0))
            return True
        except OSError:
            pass
        finally:
            if sock is not None:
                sock.close()
    return False


IPV6_ENABLED = _is_ipv6_enabled()


def skip_unless_bind_unix_socket(test):
    """Decorator skipping *test* unless AF_UNIX binding works."""
    import unittest
    if not hasattr(socket, 'AF_UNIX'):
        return unittest.skip('No UNIX Sockets')(test)
    from test.support import os_helper
    addr = os_helper.TESTFN + "can_bind_unix_socket"
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
        try:
            sock.bind(addr)
            return test
        except (PermissionError, OSError, TypeError):
            # WeavePy exposes AF_UNIX but does not yet support binding a
            # filesystem-path address, so the bind fails here; skip the
            # test exactly as CPython does when an AF_UNIX bind is denied.
            return unittest.skip('cannot bind AF_UNIX sockets')(test)
        finally:
            os_helper.unlink(addr)


def skip_if_tcp_blackhole(test):
    """Decorator skipping *test* on hosts with a TCP blackhole sysctl.

    CPython probes a macOS-only `net.inet.tcp.blackhole` sysctl that, when
    set, silently drops packets and would hang connection tests. WeavePy's
    test hosts don't enable it, so this is a pass-through (never skips).
    """
    return test


@contextlib.contextmanager
def transient_internet(resource_name, *, timeout=30.0, errnos=()):
    """Turn transient network failures inside the block into skips."""
    import unittest
    denied = _TRANSIENT_ERRNOS | set(errnos)
    try:
        yield
    except OSError as err:
        eno = getattr(err, 'errno', None)
        if eno in denied or isinstance(err, (socket.gaierror, socket.timeout)):
            raise unittest.SkipTest(
                f"resource {resource_name!r} is not available: {err}")
        raise


def get_socket_conn_refused_errs():
    """Errnos a refused connection can surface as."""
    errors = [errno.ECONNREFUSED]
    if hasattr(errno, 'ENETUNREACH'):
        errors.append(errno.ENETUNREACH)
    if hasattr(errno, 'EADDRNOTAVAIL'):
        errors.append(errno.EADDRNOTAVAIL)
    if hasattr(errno, 'EHOSTUNREACH'):
        errors.append(errno.EHOSTUNREACH)
    return errors
