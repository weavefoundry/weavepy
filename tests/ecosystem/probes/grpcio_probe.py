"""Ecosystem probe: grpcio (PyPI wheel; RFC 0072 WS4 stretch) — builds
an in-process server on a loopback port with a GenericRpcHandler using
bytes-identity serializers (no protobuf dependency), then round-trips a
unary-unary echo through a client channel with a deadline."""

from concurrent import futures

import grpc


class EchoHandler(grpc.GenericRpcHandler):
    def service(self, handler_call_details):
        if handler_call_details.method == "/echo.Echo/Shout":
            return grpc.unary_unary_rpc_method_handler(
                lambda request, context: request.upper(),
                request_deserializer=None,
                response_serializer=None,
            )
        return None


server = grpc.server(futures.ThreadPoolExecutor(max_workers=2))
server.add_generic_rpc_handlers((EchoHandler(),))
port = server.add_insecure_port("127.0.0.1:0")
assert port > 0, port
server.start()

with grpc.insecure_channel(f"127.0.0.1:{port}") as channel:
    shout = channel.unary_unary("/echo.Echo/Shout")
    reply = shout(b"weavepy echo", timeout=10)
    assert reply == b"WEAVEPY ECHO", reply

server.stop(grace=None).wait()
print("grpcio ok", grpc.__version__)
