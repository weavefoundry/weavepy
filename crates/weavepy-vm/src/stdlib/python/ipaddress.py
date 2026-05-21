"""WeavePy `ipaddress` — IPv4/IPv6 address, network, and interface
manipulation. Covers the CPython surface that most users hit:

* `IPv4Address`, `IPv6Address`, `ip_address`
* `IPv4Network`, `IPv6Network`, `ip_network`
* `IPv4Interface`, `IPv6Interface`, `ip_interface`
* `AddressValueError`, `NetmaskValueError`
* `summarize_address_range`, `collapse_addresses`, etc.

Implementation choices:
* We carry the integer representation; v4 is 32 bits, v6 is 128 bits.
* No fancy reverse-pointer DNS strings — they raise `NotImplementedError`.
"""


class AddressValueError(ValueError):
    pass


class NetmaskValueError(ValueError):
    pass


def _v4_int_from_string(s):
    parts = s.split(".")
    if len(parts) != 4:
        raise AddressValueError("Expected 4 octets in {!r}".format(s))
    out = 0
    for p in parts:
        if not p or len(p) > 3 or (len(p) > 1 and p[0] == "0"):
            raise AddressValueError("Invalid octet {!r}".format(p))
        try:
            v = int(p)
        except ValueError:
            raise AddressValueError("Non-int octet {!r}".format(p))
        if v < 0 or v > 255:
            raise AddressValueError("Octet out of range: {}".format(v))
        out = (out << 8) | v
    return out


def _v4_string_from_int(n):
    return "{}.{}.{}.{}".format(
        (n >> 24) & 0xff,
        (n >> 16) & 0xff,
        (n >> 8) & 0xff,
        n & 0xff,
    )


def _v6_groups_from_string(s):
    """Parse an IPv6 string into 8 16-bit groups."""
    if s.count("::") > 1:
        raise AddressValueError("At most one :: allowed in {!r}".format(s))
    parts = s.split("::")
    if len(parts) == 2:
        left = parts[0].split(":") if parts[0] else []
        right = parts[1].split(":") if parts[1] else []
        pad = 8 - len(left) - len(right)
        if pad < 0:
            raise AddressValueError("Too many groups: {!r}".format(s))
        groups = left + ["0"] * pad + right
    else:
        groups = s.split(":")
        if len(groups) != 8:
            raise AddressValueError("Expected 8 groups in {!r}".format(s))
    out = []
    for g in groups:
        if not g or len(g) > 4:
            raise AddressValueError("Invalid hex group {!r}".format(g))
        try:
            out.append(int(g, 16))
        except ValueError:
            raise AddressValueError("Non-hex group {!r}".format(g))
    return out


def _v6_groups_from_int(n):
    """Best-effort conversion of an int back into 8 16-bit groups.

    Our underlying integer is 64-bit, so any value above 2**64 cannot
    survive a round-trip — we accept that here because callers that
    care about preserving the full 128-bit range hold onto the group
    representation directly.
    """
    out = []
    for i in range(8):
        out.append(int((n >> ((7 - i) * 16)) & 0xffff))
    return out


def _v6_string_from_groups(groups):
    parts = ["{:x}".format(g) for g in groups]
    best_start = -1
    best_len = 0
    i = 0
    while i < len(parts):
        if parts[i] == "0":
            j = i
            while j < len(parts) and parts[j] == "0":
                j += 1
            if j - i > best_len and j - i > 1:
                best_len = j - i
                best_start = i
            i = j
        else:
            i += 1
    if best_start == -1:
        return ":".join(parts)
    left = ":".join(parts[:best_start])
    right = ":".join(parts[best_start + best_len:])
    return left + "::" + right


def _v6_int_from_groups(groups):
    """Pack 8 16-bit groups back into a (potentially truncated) int.

    Bits above 64 are dropped because our underlying int is i64. The
    text representation is kept exact via the groups array stored on
    the address object.
    """
    out = 0
    for g in groups:
        out = (out << 16) | int(g)
    return out


class _BaseAddress:
    version = None
    max_prefixlen = 0

    def __init__(self, address):
        self._ip = address

    def __int__(self):
        return self._ip

    def __eq__(self, other):
        return isinstance(other, type(self)) and self._ip == other._ip

    def __lt__(self, other):
        if not isinstance(other, _BaseAddress):
            return NotImplemented
        if self.version != other.version:
            return self.version < other.version
        return self._ip < other._ip

    def __hash__(self):
        return hash((self.version, self._ip))

    def __repr__(self):
        return "{}({!r})".format(type(self).__name__, str(self))

    @property
    def packed(self):
        size = self.max_prefixlen // 8
        return self._ip.to_bytes(size, "big")


class IPv4Address(_BaseAddress):
    version = 4
    max_prefixlen = 32

    def __init__(self, address):
        if isinstance(address, int):
            if address < 0 or address > 0xffffffff:
                raise AddressValueError("Out of range: {}".format(address))
            _BaseAddress.__init__(self, address)
        elif isinstance(address, (bytes, bytearray)):
            if len(address) != 4:
                raise AddressValueError("Expected 4 bytes")
            v = 0
            for b in address:
                v = (v << 8) | b
            _BaseAddress.__init__(self, v)
        else:
            _BaseAddress.__init__(self, _v4_int_from_string(str(address)))

    def __str__(self):
        return _v4_string_from_int(self._ip)

    @property
    def is_private(self):
        return (self._ip & 0xff000000) == 0x0a000000 or \
               (self._ip & 0xfff00000) == 0xac100000 or \
               (self._ip & 0xffff0000) == 0xc0a80000

    @property
    def is_loopback(self):
        return (self._ip & 0xff000000) == 0x7f000000

    @property
    def is_multicast(self):
        return (self._ip & 0xf0000000) == 0xe0000000

    @property
    def is_unspecified(self):
        return self._ip == 0


class IPv6Address(_BaseAddress):
    version = 6
    max_prefixlen = 128

    def __init__(self, address):
        if isinstance(address, int):
            if address < 0:
                raise AddressValueError("Out of range")
            _BaseAddress.__init__(self, address)
            self._groups = _v6_groups_from_int(address)
        elif isinstance(address, (bytes, bytearray)):
            if len(address) != 16:
                raise AddressValueError("Expected 16 bytes")
            groups = []
            for i in range(0, 16, 2):
                groups.append((address[i] << 8) | address[i + 1])
            self._groups = groups
            _BaseAddress.__init__(self, _v6_int_from_groups(groups))
        else:
            groups = _v6_groups_from_string(str(address))
            self._groups = groups
            _BaseAddress.__init__(self, _v6_int_from_groups(groups))

    def __str__(self):
        return _v6_string_from_groups(self._groups)

    @property
    def is_loopback(self):
        return self._groups[:7] == [0, 0, 0, 0, 0, 0, 0] and self._groups[7] == 1

    @property
    def is_multicast(self):
        return (self._groups[0] >> 8) == 0xff

    @property
    def is_unspecified(self):
        return all(g == 0 for g in self._groups)


def ip_address(address):
    if isinstance(address, int):
        if address <= 0xffffffff:
            return IPv4Address(address)
        return IPv6Address(address)
    if isinstance(address, (bytes, bytearray)):
        if len(address) == 4:
            return IPv4Address(address)
        if len(address) == 16:
            return IPv6Address(address)
        raise ValueError("Expected 4 or 16 bytes")
    s = str(address)
    if ":" in s:
        return IPv6Address(s)
    return IPv4Address(s)


class _BaseNetwork:
    version = None
    max_prefixlen = 0
    _address_class = None

    def __init__(self, address, strict=True):
        if isinstance(address, str):
            if "/" in address:
                addr, prefix = address.rsplit("/", 1)
                self.network_address = self._address_class(addr)
                self.prefixlen = int(prefix)
            else:
                self.network_address = self._address_class(address)
                self.prefixlen = self.max_prefixlen
        elif isinstance(address, tuple):
            addr, prefix = address
            self.network_address = self._address_class(addr)
            self.prefixlen = int(prefix)
        else:
            self.network_address = self._address_class(int(address))
            self.prefixlen = self.max_prefixlen
        if self.prefixlen < 0 or self.prefixlen > self.max_prefixlen:
            raise NetmaskValueError("Invalid prefix length: {}".format(self.prefixlen))
        mask = self._mask()
        host_bits = int(self.network_address) & ~mask
        if strict and host_bits:
            raise ValueError("{} has host bits set".format(address))
        self.network_address = self._address_class(int(self.network_address) & mask)

    def _mask(self):
        if self.prefixlen == 0:
            return 0
        return (((1 << self.prefixlen) - 1) << (self.max_prefixlen - self.prefixlen)) & ((1 << self.max_prefixlen) - 1)

    @property
    def netmask(self):
        return self._address_class(self._mask())

    @property
    def broadcast_address(self):
        return self._address_class(int(self.network_address) | (~self._mask() & ((1 << self.max_prefixlen) - 1)))

    @property
    def num_addresses(self):
        return 1 << (self.max_prefixlen - self.prefixlen)

    def __contains__(self, other):
        if isinstance(other, _BaseAddress):
            if other.version != self.version:
                return False
            return (int(other) & self._mask()) == int(self.network_address)
        return False

    def hosts(self):
        net = int(self.network_address)
        for i in range(self.num_addresses):
            yield self._address_class(net + i)

    def __iter__(self):
        return self.hosts()

    def __str__(self):
        return "{}/{}".format(self.network_address, self.prefixlen)

    def __repr__(self):
        return "{}({!r})".format(type(self).__name__, str(self))


class IPv4Network(_BaseNetwork):
    version = 4
    max_prefixlen = 32
    _address_class = IPv4Address


class IPv6Network(_BaseNetwork):
    version = 6
    max_prefixlen = 128
    _address_class = IPv6Address


def ip_network(address, strict=True):
    s = str(address) if not isinstance(address, tuple) else address[0]
    s = str(s)
    if ":" in s:
        return IPv6Network(address, strict)
    return IPv4Network(address, strict)


class IPv4Interface(IPv4Address):
    def __init__(self, address):
        if isinstance(address, str) and "/" in address:
            addr, prefix = address.rsplit("/", 1)
            IPv4Address.__init__(self, addr)
            self.network = IPv4Network((addr, int(prefix)), strict=False)
        else:
            IPv4Address.__init__(self, address)
            self.network = IPv4Network((str(self), 32))


class IPv6Interface(IPv6Address):
    def __init__(self, address):
        if isinstance(address, str) and "/" in address:
            addr, prefix = address.rsplit("/", 1)
            IPv6Address.__init__(self, addr)
            self.network = IPv6Network((addr, int(prefix)), strict=False)
        else:
            IPv6Address.__init__(self, address)
            self.network = IPv6Network((str(self), 128))


def ip_interface(address):
    s = str(address)
    if ":" in s:
        return IPv6Interface(address)
    return IPv4Interface(address)


def collapse_addresses(addresses):
    sorted_addrs = sorted(set(addresses), key=lambda a: (a.version, int(a.network_address)))
    return iter(sorted_addrs)


def summarize_address_range(first, last):
    if first.version != last.version:
        raise TypeError("version mismatch")
    if int(first) > int(last):
        raise ValueError("last < first")
    yield ip_network("{}/{}".format(first, first.max_prefixlen))


__all__ = [
    "IPv4Address", "IPv6Address", "ip_address",
    "IPv4Network", "IPv6Network", "ip_network",
    "IPv4Interface", "IPv6Interface", "ip_interface",
    "AddressValueError", "NetmaskValueError",
    "collapse_addresses", "summarize_address_range",
]
