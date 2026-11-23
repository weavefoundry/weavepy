"""SHA-1 and SHA-2 digests match the NIST test vectors however data arrives.

`sha1`, `sha224`, `sha256`, `sha384` and `sha512` of the FIPS 180 vectors,
fed at once, in chunks that straddle the block size, and through
`hashlib.new`; `copy()` gives an independent hasher, `digest()` doesn't
finish the hasher it is asked of, and the sizes and names are CPython's.
"""

import hashlib
import unittest

ABC = {
    "sha1": "a9993e364706816aba3e25717850c26c9cd0d89d",
    "sha224": "23097d223405d8228642a477bda255b32aadbce4bda0b3f7e36c9da7",
    "sha256": "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
    "sha384": "cb00753f45a35e8bb5a03d699ac65007272c32ab0eded1631a8b605a43ff5bed"
              "8086072ba1e7cc2358baeca134c825a7",
    "sha512": "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a"
              "2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f",
}
MILLION_A = {
    "sha1": "34aa973cd4c4daa4f61eeb2bdbad27316534016f",
    "sha256": "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0",
    "sha512": "e718483d0ce769644e2e42c7bc15b4638e1f98b13b2044285632a803afa973eb"
              "de0ff244877ea60a4cb0432ce577c31beb009c5c2c49aa2e4eadb217ad8cc09b",
}
SIZES = {"sha1": (20, 64), "sha224": (28, 64), "sha256": (32, 64), "sha384": (48, 128),
         "sha512": (64, 128)}


class ShaTests(unittest.TestCase):
    def test_vectors(self):
        for name, want in ABC.items():
            self.assertEqual(getattr(hashlib, name)(b"abc").hexdigest(), want)
            self.assertEqual(hashlib.new(name, b"abc").hexdigest(), want)
            h = hashlib.new(name)
            self.assertEqual((h.name, h.digest_size, h.block_size), (name,) + SIZES[name])
        data = b"a" * 1_000_000
        for name, want in MILLION_A.items():
            self.assertEqual(getattr(hashlib, name)(data).hexdigest(), want)
            h = getattr(hashlib, name)()
            for at in range(0, len(data), 999):
                h.update(data[at:at + 999])
            self.assertEqual(h.hexdigest(), want)

    def test_chunks_copy_and_repeat(self):
        blob = bytes(range(256)) * 9
        for name in ABC:
            whole = getattr(hashlib, name)(blob).digest()
            for step in (1, 63, 64, 65, 127, 128, 129, 1000):
                h = getattr(hashlib, name)()
                for at in range(0, len(blob), step):
                    h.update(memoryview(blob)[at:at + step])
                self.assertEqual(h.digest(), whole, (name, step))
                self.assertEqual(h.digest(), whole)
            h = getattr(hashlib, name)(blob[:100])
            c = h.copy()
            c.update(blob[100:])
            self.assertEqual(c.digest(), whole)
            self.assertEqual(h.digest(), getattr(hashlib, name)(blob[:100]).digest())
            h.update(blob[100:])
            self.assertEqual(h.hexdigest(), whole.hex())

    def test_live_hashers_outlast_dead_ones(self):
        keep = [hashlib.sha256(b"keep %d" % i) for i in range(50)]
        for i in range(5000):
            hashlib.sha256(b"x").digest()
            hashlib.md5(b"y").hexdigest()
            if i % 100 == 0:
                keep[i // 100].update(b"!")
        for i, h in enumerate(keep):
            want = b"keep %d" % i + (b"!" if i < 50 else b"")
            self.assertEqual(h.hexdigest(), hashlib.sha256(want).hexdigest(), i)
            self.assertEqual(h.copy().digest(), h.digest())

    def test_updates_in_a_loop(self):
        ba = bytearray(b"0123456789abcdef")
        for name in ("sha1", "sha256", "sha512", "md5"):
            h = hashlib.new(name)
            for i in range(3000):
                h.update(ba if i % 3 else memoryview(ba)[2:9])
                if i % 1000 == 0:
                    h.hexdigest()
            want = hashlib.new(name, (bytes(ba[2:9]) + bytes(ba) * 2) * 1000).hexdigest()
            self.assertEqual(h.hexdigest(), want, name)
            with self.assertRaises(TypeError):
                h.update("text")


if __name__ == "__main__":
    unittest.main()
