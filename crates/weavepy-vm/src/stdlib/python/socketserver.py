"""WeavePy `socketserver` — TCP / UDP server framework.

Provides:
* `BaseServer`, `TCPServer`, `UDPServer` — synchronous servers.
* `ThreadingMixIn`, `ForkingMixIn` — concurrency mixins. (Threading
  uses the cooperative threading module; forking is a no-op.)
* `BaseRequestHandler`, `StreamRequestHandler`, `DatagramRequestHandler`.
"""

import socket as _socket


__all__ = [
    "BaseServer", "TCPServer", "UDPServer",
    "ThreadingMixIn", "ForkingMixIn",
    "ThreadingTCPServer", "ThreadingUDPServer",
    "ForkingTCPServer", "ForkingUDPServer",
    "BaseRequestHandler", "StreamRequestHandler", "DatagramRequestHandler",
]


class BaseServer:
    """Shared infrastructure for socket servers."""

    address_family = _socket.AF_INET
    socket_type = _socket.SOCK_STREAM
    request_queue_size = 5
    allow_reuse_address = False
    timeout = None

    def __init__(self, server_address, RequestHandlerClass):
        self.server_address = server_address
        self.RequestHandlerClass = RequestHandlerClass
        self.__shutdown_request = False

    def serve_forever(self, poll_interval=0.5):
        self.__shutdown_request = False
        while not self.__shutdown_request:
            self.handle_request()

    def shutdown(self):
        self.__shutdown_request = True

    def handle_request(self):
        request, client_address = self.get_request()
        try:
            self.process_request(request, client_address)
        except Exception:
            try:
                self.shutdown_request(request)
            finally:
                raise

    def process_request(self, request, client_address):
        try:
            self.finish_request(request, client_address)
        finally:
            self.shutdown_request(request)

    def finish_request(self, request, client_address):
        self.RequestHandlerClass(request, client_address, self)

    def server_bind(self):
        if self.allow_reuse_address:
            try:
                self.socket.setsockopt(_socket.SOL_SOCKET, _socket.SO_REUSEADDR, 1)
            except OSError:
                pass
        self.socket.bind(self.server_address)
        self.server_address = self.socket.getsockname()

    def server_activate(self):
        pass

    def server_close(self):
        try:
            self.socket.close()
        except Exception:
            pass

    def shutdown_request(self, request):
        try:
            request.shutdown(_socket.SHUT_WR)
        except Exception:
            pass
        self.close_request(request)

    def close_request(self, request):
        try:
            request.close()
        except Exception:
            pass

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.server_close()
        return False


class TCPServer(BaseServer):
    """A synchronous TCP/IP server."""

    def __init__(self, server_address, RequestHandlerClass, bind_and_activate=True):
        BaseServer.__init__(self, server_address, RequestHandlerClass)
        self.socket = _socket.socket(self.address_family, self.socket_type)
        if bind_and_activate:
            try:
                self.server_bind()
                self.server_activate()
            except Exception:
                self.server_close()
                raise

    def server_activate(self):
        self.socket.listen(self.request_queue_size)

    def get_request(self):
        return self.socket.accept()


class UDPServer(TCPServer):
    """A synchronous UDP server (request is `(data, socket)`)."""

    socket_type = _socket.SOCK_DGRAM
    max_packet_size = 8192

    def server_activate(self):
        pass

    def get_request(self):
        data, addr = self.socket.recvfrom(self.max_packet_size)
        return (data, self.socket), addr

    def shutdown_request(self, request):
        self.close_request(request)

    def close_request(self, request):
        pass


class ThreadingMixIn:
    daemon_threads = False
    block_on_close = True

    def process_request(self, request, client_address):
        # The cooperative thread runtime simply runs the request inline
        # for now; this gives the same semantics as a thread joined
        # immediately afterwards.
        TCPServer.process_request(self, request, client_address)


class ForkingMixIn:
    max_children = 40

    def process_request(self, request, client_address):
        TCPServer.process_request(self, request, client_address)


class ThreadingTCPServer(ThreadingMixIn, TCPServer):
    pass


class ThreadingUDPServer(ThreadingMixIn, UDPServer):
    pass


class ForkingTCPServer(ForkingMixIn, TCPServer):
    pass


class ForkingUDPServer(ForkingMixIn, UDPServer):
    pass


class BaseRequestHandler:
    def __init__(self, request, client_address, server):
        self.request = request
        self.client_address = client_address
        self.server = server
        self.setup()
        try:
            self.handle()
        finally:
            self.finish()

    def setup(self):
        pass

    def handle(self):
        pass

    def finish(self):
        pass


class StreamRequestHandler(BaseRequestHandler):
    rbufsize = -1
    wbufsize = 0
    timeout = None
    disable_nagle_algorithm = False

    def setup(self):
        self.connection = self.request

    def finish(self):
        try:
            self.connection.close()
        except Exception:
            pass


class DatagramRequestHandler(BaseRequestHandler):
    def setup(self):
        self.packet, self.socket = self.request

    def finish(self):
        pass
