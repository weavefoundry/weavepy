"""Tiny pure-Python AES-style XOR scrambler. Not real AES — a
fixed-shape byte-and-XOR loop that stresses string slicing and
list-of-int arithmetic."""

import os


def _scramble(plain, key):
    out = []
    klen = len(key)
    for i, c in enumerate(plain):
        out.append((c ^ key[i % klen]) & 0xFF)
    return bytes(out)


def bench(n):
    plain = bytes(range(256)) * 4  # 1024 bytes
    key = bytes(range(16))
    last = b""
    for _ in range(n):
        last = _scramble(plain, key)
    return len(last)


if __name__ == "__main__":
    n = int(os.environ.get("WEAVEPY_BENCH_WORK", "10"))
    bench(n)
