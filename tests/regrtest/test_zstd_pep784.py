"""PEP 784 `compression.zstd` surface (RFC 0076 WS15).

Scoped from CPython 3.14's test_zstd.py to the documented public
surface: one-shot and incremental (de)compression, modes, max_length,
frame introspection, dictionaries (trained, finalized, raw, prefix),
options dicts, the parameter/strategy enums, ZstdFile, open(), and the
`compression.*` re-export shims.
"""

import io
import os
import tempfile
import unittest

from compression import zstd
from compression.zstd import (
    COMPRESSION_LEVEL_DEFAULT,
    CompressionParameter,
    DecompressionParameter,
    Strategy,
    ZstdCompressor,
    ZstdDecompressor,
    ZstdDict,
    ZstdError,
    ZstdFile,
    compress,
    decompress,
    finalize_dict,
    get_frame_info,
    get_frame_size,
    open as zstd_open,
    train_dict,
    zstd_version,
    zstd_version_info,
)

DATA = b"WeavePy zstandard fixture payload " * 512
SAMPLES = [bytes([i % 251]) * 64 + b" common tail for training" for i in range(80)]


class OneShotTests(unittest.TestCase):
    def test_round_trip(self):
        frame = compress(DATA)
        self.assertLess(len(frame), len(DATA) // 4)
        self.assertEqual(decompress(frame), DATA)

    def test_levels(self):
        fast = compress(DATA, level=1)
        strong = compress(DATA, level=19)
        self.assertEqual(decompress(fast), DATA)
        self.assertEqual(decompress(strong), DATA)
        self.assertLessEqual(len(strong), len(fast))

    def test_default_level_constant(self):
        self.assertIsInstance(COMPRESSION_LEVEL_DEFAULT, int)
        self.assertEqual(COMPRESSION_LEVEL_DEFAULT, 3)

    def test_multi_frame_decompress(self):
        two = compress(DATA[: len(DATA) // 2]) + compress(DATA[len(DATA) // 2 :])
        self.assertEqual(decompress(two), DATA)

    def test_truncated_input_raises(self):
        frame = compress(DATA)
        with self.assertRaises(ZstdError):
            decompress(frame[: len(frame) // 2])

    def test_garbage_raises(self):
        with self.assertRaises(ZstdError):
            decompress(b"not a zstd frame at all")

    def test_version(self):
        self.assertIsInstance(zstd_version, str)
        parts = tuple(int(p) for p in zstd_version.split("."))
        self.assertEqual(parts, zstd_version_info)
        self.assertGreaterEqual(zstd_version_info, (1, 4, 5))


class CompressorTests(unittest.TestCase):
    def test_incremental_modes(self):
        comp = ZstdCompressor()
        out = comp.compress(DATA[:1000])
        out += comp.compress(DATA[1000:4000], mode=ZstdCompressor.CONTINUE)
        self.assertEqual(comp.last_mode, ZstdCompressor.CONTINUE)
        out += comp.compress(DATA[4000:8000], mode=ZstdCompressor.FLUSH_BLOCK)
        self.assertEqual(comp.last_mode, ZstdCompressor.FLUSH_BLOCK)
        out += comp.compress(DATA[8000:], mode=ZstdCompressor.FLUSH_FRAME)
        self.assertEqual(comp.last_mode, ZstdCompressor.FLUSH_FRAME)
        self.assertEqual(decompress(out), DATA)

    def test_flush_block_is_readable_midstream(self):
        comp = ZstdCompressor()
        chunk = comp.compress(DATA, mode=ZstdCompressor.FLUSH_BLOCK)
        d = ZstdDecompressor()
        self.assertEqual(d.decompress(chunk), DATA)
        self.assertFalse(d.eof)
        comp.flush()

    def test_flush_mode_validation(self):
        comp = ZstdCompressor()
        with self.assertRaises(ValueError):
            comp.flush(mode=ZstdCompressor.CONTINUE)
        with self.assertRaises(ValueError):
            comp.compress(b"x", mode=17)

    def test_two_frames_from_one_compressor(self):
        comp = ZstdCompressor()
        first = comp.compress(b"frame one", mode=ZstdCompressor.FLUSH_FRAME)
        second = comp.compress(b"frame two", mode=ZstdCompressor.FLUSH_FRAME)
        self.assertEqual(decompress(first + second), b"frame oneframe two")

    def test_set_pledged_input_size(self):
        comp = ZstdCompressor()
        comp.set_pledged_input_size(len(DATA))
        frame = comp.compress(DATA, mode=ZstdCompressor.FLUSH_FRAME)
        self.assertEqual(get_frame_info(frame).decompressed_size, len(DATA))
        # Mid-frame pledges are rejected.
        comp.compress(b"partial")
        with self.assertRaises(ValueError):
            comp.set_pledged_input_size(100)
        comp.flush()

    def test_options_dict(self):
        options = {
            CompressionParameter.compression_level: 12,
            CompressionParameter.checksum_flag: 1,
            CompressionParameter.window_log: 20,
        }
        frame = compress(DATA, options=options)
        self.assertEqual(decompress(frame), DATA)
        with self.assertRaises(ZstdError):
            compress(DATA, options={CompressionParameter.window_log: 9999})


class DecompressorTests(unittest.TestCase):
    def test_incremental(self):
        frame = compress(DATA)
        d = ZstdDecompressor()
        out = d.decompress(frame[:11])
        self.assertFalse(d.eof)
        self.assertTrue(d.needs_input)
        out += d.decompress(frame[11:])
        self.assertEqual(out, DATA)
        self.assertTrue(d.eof)
        self.assertFalse(d.needs_input)
        self.assertEqual(d.unused_data, b"")

    def test_max_length(self):
        frame = compress(DATA)
        d = ZstdDecompressor()
        head = d.decompress(frame, max_length=100)
        self.assertEqual(len(head), 100)
        self.assertFalse(d.needs_input)
        rest = d.decompress(b"")
        self.assertEqual(head + rest, DATA)
        self.assertTrue(d.eof)

    def test_unused_data_and_single_frame_contract(self):
        frame = compress(DATA)
        trailing = b"TRAILING BYTES"
        d = ZstdDecompressor()
        out = d.decompress(frame + trailing)
        self.assertEqual(out, DATA)
        self.assertTrue(d.eof)
        self.assertEqual(d.unused_data, trailing)
        with self.assertRaises(EOFError):
            d.decompress(b"more")

    def test_window_log_max_option(self):
        frame = compress(DATA)
        d = ZstdDecompressor(options={DecompressionParameter.window_log_max: 25})
        self.assertEqual(d.decompress(frame), DATA)


class FrameTests(unittest.TestCase):
    def test_get_frame_info(self):
        frame = compress(DATA)
        info = get_frame_info(frame)
        self.assertEqual(info.decompressed_size, len(DATA))
        self.assertEqual(info.dictionary_id, 0)
        self.assertIn("FrameInfo", repr(info))
        with self.assertRaises(AttributeError):
            info.decompressed_size = 1

    def test_get_frame_size(self):
        frame = compress(DATA)
        self.assertEqual(get_frame_size(frame + b"garbage after frame"), len(frame))
        with self.assertRaises(ZstdError):
            get_frame_size(b"junk")


class DictTests(unittest.TestCase):
    def test_train_and_round_trip(self):
        zd = train_dict(SAMPLES, 4096)
        self.assertIsInstance(zd, ZstdDict)
        self.assertGreater(zd.dict_id, 0)
        self.assertLessEqual(len(zd.dict_content), 4096)
        frame = compress(DATA, zstd_dict=zd)
        self.assertEqual(decompress(frame, zstd_dict=zd), DATA)
        self.assertEqual(get_frame_info(frame).dictionary_id, zd.dict_id)

    def test_train_validation(self):
        with self.assertRaises(TypeError):
            train_dict(SAMPLES, "big")
        with self.assertRaises(ValueError):
            train_dict([b"", b""], 4096)

    def test_finalize(self):
        base = train_dict(SAMPLES, 4096)
        final = finalize_dict(base, SAMPLES, 4096, 3)
        self.assertIsInstance(final, ZstdDict)
        self.assertGreater(final.dict_id, 0)
        frame = compress(DATA, zstd_dict=final)
        self.assertEqual(decompress(frame, zstd_dict=final), DATA)
        with self.assertRaises(TypeError):
            finalize_dict(b"raw bytes", SAMPLES, 4096, 3)

    def test_raw_dict(self):
        raw = ZstdDict(b"x" * 64, is_raw=True)
        self.assertEqual(raw.dict_id, 0)
        with self.assertRaises(ValueError):
            ZstdDict(b"x" * 64)  # no magic, not marked raw

    def test_prefix(self):
        zd = train_dict(SAMPLES, 4096)
        frame = compress(DATA, zstd_dict=zd.as_prefix)
        self.assertEqual(decompress(frame, zstd_dict=zd.as_prefix), DATA)
        # Digested/undigested advice tuples load like the plain dict.
        frame = compress(DATA, zstd_dict=zd.as_digested_dict)
        self.assertEqual(decompress(frame, zstd_dict=zd.as_undigested_dict), DATA)


class EnumTests(unittest.TestCase):
    def test_strategy_order(self):
        members = list(Strategy)
        self.assertEqual(members[0], Strategy.fast)
        self.assertEqual(members[-1], Strategy.btultra2)
        self.assertEqual([m.value for m in members],
                         sorted(m.value for m in members))

    def test_bounds(self):
        lo, hi = CompressionParameter.compression_level.bounds()
        self.assertLess(lo, 0)
        self.assertGreaterEqual(hi, 19)
        lo, hi = CompressionParameter.strategy.bounds()
        self.assertEqual((lo, hi), (Strategy.fast, Strategy.btultra2))
        lo, hi = DecompressionParameter.window_log_max.bounds()
        self.assertLess(lo, hi)

    def test_strategy_as_option(self):
        frame = compress(
            DATA, options={CompressionParameter.strategy: Strategy.btopt}
        )
        self.assertEqual(decompress(frame), DATA)


class FileTests(unittest.TestCase):
    def test_bytesio_round_trip(self):
        buf = io.BytesIO()
        with ZstdFile(buf, "w") as f:
            f.write(DATA)
        buf.seek(0)
        with ZstdFile(buf) as f:
            self.assertEqual(f.read(), DATA)

    def test_named_file_and_append(self):
        with tempfile.TemporaryDirectory() as td:
            path = os.path.join(td, "payload.zst")
            with ZstdFile(path, "w", level=7) as f:
                f.write(DATA[:4000])
            with ZstdFile(path, "a") as f:
                f.write(DATA[4000:])
            with ZstdFile(path) as f:
                self.assertEqual(f.read(), DATA)

    def test_read_modes_and_seek(self):
        buf = io.BytesIO()
        with ZstdFile(buf, "w") as f:
            f.write(DATA)
        buf.seek(0)
        with ZstdFile(buf, "r") as f:
            self.assertEqual(f.read(100), DATA[:100])
            f.seek(0)
            self.assertEqual(f.read(), DATA)
            f.seek(-50, io.SEEK_END)
            self.assertEqual(f.read(), DATA[-50:])

    def test_mode_validation(self):
        with self.assertRaises(ValueError):
            ZstdFile(io.BytesIO(), "q")
        with self.assertRaises(TypeError):
            ZstdFile(io.BytesIO(), "r", level=3)

    def test_open_text(self):
        text = "line one\nline twö\n" * 100
        buf = io.BytesIO()
        with zstd_open(buf, "wt", encoding="utf-8") as f:
            f.write(text)
        buf.seek(0)
        with zstd_open(buf, "rt", encoding="utf-8") as f:
            self.assertEqual(f.read(), text)

    def test_flush_block_through_file(self):
        buf = io.BytesIO()
        with ZstdFile(buf, "w") as f:
            f.write(DATA[:1000])
            f.flush(f.FLUSH_BLOCK)
            mid = buf.getvalue()
            self.assertTrue(mid)
            f.write(DATA[1000:])
        buf.seek(0)
        with ZstdFile(buf) as f:
            self.assertEqual(f.read(), DATA)


class ShimTests(unittest.TestCase):
    def test_reexport_modules(self):
        import bz2
        import gzip
        import lzma
        import zlib

        import compression.bz2
        import compression.gzip
        import compression.lzma
        import compression.zlib

        self.assertIs(compression.bz2.BZ2File, bz2.BZ2File)
        self.assertIs(compression.gzip.GzipFile, gzip.GzipFile)
        self.assertIs(compression.lzma.LZMAFile, lzma.LZMAFile)
        self.assertIs(compression.zlib.compress, zlib.compress)


if __name__ == "__main__":
    unittest.main()
