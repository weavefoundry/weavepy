import socket

print("--- constants ---")
print("AF_INET:", socket.AF_INET)
print("SOCK_STREAM:", socket.SOCK_STREAM)
print("SOL_SOCKET is int:", isinstance(socket.SOL_SOCKET, int))

print("--- helpers ---")
print("htons(80):", socket.htons(80))
print("ntohs(htons(80)):", socket.ntohs(socket.htons(80)))
print("inet_aton('127.0.0.1'):", socket.inet_aton('127.0.0.1'))
print("inet_ntoa:", socket.inet_ntoa(socket.inet_aton('192.168.0.1')))

print("--- getaddrinfo ---")
info = socket.getaddrinfo("127.0.0.1", 80, socket.AF_INET, socket.SOCK_STREAM)
print("len:", len(info))
fam, kind, proto, canon, addr = info[0]
print("family:", fam == socket.AF_INET, "kind:", kind == socket.SOCK_STREAM)
print("addr:", addr)

print("--- TCP echo ---")
srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
srv.bind(("127.0.0.1", 0))
srv.listen(1)
host, port = srv.getsockname()
print("listening on:", host, "with port>0:", port > 0)

c = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
c.connect((host, port))
conn, peer = srv.accept()
print("accepted from:", peer[0])

c.sendall(b"ping\n")
got = conn.recv(64)
print("server got:", got)
conn.sendall(b"pong\n")
echoed = c.recv(64)
print("client got:", echoed)

c.close()
conn.close()
srv.close()
