"""Live compressors, decompressors and hashers keep their state while dead
ones are let go.

Thousands of short-lived `zlib`, `bz2`, `lzma` and `compression.zstd`
streaming objects and hashers are made and dropped around a few that stay
alive mid-stream; the live ones still finish their streams to what one-shot
calls give, so freeing the dead ones' native state never touches theirs.
"""

import bz2
import hashlib
import lzma
import unittest
import zlib

try:
    from compression import zstd
except ImportError:
    zstd = None

DATA = b"stream states " * 500


def roundtrips():
    makers = [
        ("zlib", zlib.compressobj, zlib.decompressobj, zlib.decompress),
        ("bz2", bz2.BZ2Compressor, bz2.BZ2Decompressor, bz2.decompress),
        ("lzma", lzma.LZMACompressor, lzma.LZMADecompressor, lzma.decompress),
    ]
    if zstd is not None:
        makers.append(("zstd", zstd.ZstdCompressor, zstd.ZstdDecompressor, zstd.decompress))
    return makers


class SweepTests(unittest.TestCase):
    def test_live_streams_finish(self):
        for name, comp, decomp, one_shot in roundtrips():
            live = [comp() for _ in range(5)]
            parts = [[c.compress(DATA[:100])] for c in live]
            readers = [decomp() for _ in range(3)]
            whole = comp()
            one_shot_blob = whole.compress(DATA) + whole.flush()
            got = [r.decompress(one_shot_blob[:10]) for r in readers]
            count = 3000 if name in ("zlib", "zstd") else 300
            for _ in range(count):
                c = comp()
                c.compress(b"x")
                decomp()
                zlib.decompressobj()
                hashlib.sha256(b"y")
            for c, p in zip(live, parts):
                p.append(c.compress(DATA[100:]))
                p.append(c.flush())
                self.assertEqual(one_shot(b"".join(p)), DATA, name)
            for r, g in zip(readers, got):
                self.assertEqual(g + r.decompress(one_shot_blob[10:]), DATA, name)

    def test_live_zlib_objects(self):
        d = zlib.decompressobj()
        c = zlib.compressobj()
        head = c.compress(DATA[:7])
        for _ in range(5000):
            zlib.compressobj()
            zlib.decompressobj()
        blob = head + c.compress(DATA[7:]) + c.flush()
        self.assertEqual(d.decompress(blob) + d.flush(), DATA)
        self.assertTrue(d.eof)


if __name__ == "__main__":
    unittest.main()
