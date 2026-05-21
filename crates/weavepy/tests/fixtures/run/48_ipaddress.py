import ipaddress

a = ipaddress.IPv4Address("192.168.1.10")
print("v4:", a, int(a))
print("private:", a.is_private)
print("loopback:", a.is_loopback)

b = ipaddress.IPv4Address(0x0a000001)
print("v4 from int:", b)

n = ipaddress.IPv4Network("10.0.0.0/8")
print("net:", n, "broadcast:", n.broadcast_address, "num:", n.num_addresses)
print("net contains 10.1.2.3:", ipaddress.IPv4Address("10.1.2.3") in n)
print("net contains 11.1.2.3:", ipaddress.IPv4Address("11.1.2.3") in n)

v6 = ipaddress.IPv6Address("::1")
print("v6:", v6, "loopback:", v6.is_loopback)

generic = ipaddress.ip_address("172.16.0.1")
print("generic:", generic, "version:", generic.version)

inet = ipaddress.IPv4Interface("192.168.1.10/24")
print("interface:", inet, "network:", inet.network)
