"""Public ``lzma`` module (RFC 0019).

Thin wrapper over the Rust-backed ``_lzma`` core that exposes
``compress``, ``decompress``, ``open``, and ``LZMAFile``.
"""

import _lzma
import io

_builtin_open = open

FORMAT_AUTO = _lzma.FORMAT_AUTO
FORMAT_XZ = _lzma.FORMAT_XZ
FORMAT_ALONE = _lzma.FORMAT_ALONE
FORMAT_RAW = _lzma.FORMAT_RAW

CHECK_NONE = _lzma.CHECK_NONE
CHECK_CRC32 = _lzma.CHECK_CRC32
CHECK_CRC64 = _lzma.CHECK_CRC64
CHECK_SHA256 = _lzma.CHECK_SHA256

PRESET_DEFAULT = _lzma.PRESET_DEFAULT
PRESET_EXTREME = _lzma.PRESET_EXTREME


class LZMAError(Exception):
    """Raised on LZMA compression/decompression errors."""


def compress(data, format=FORMAT_XZ, check=-1, preset=None, filters=None):
    if preset is None:
        preset = PRESET_DEFAULT
    return _lzma.compress(data, preset)


def decompress(data, format=FORMAT_AUTO, memlimit=None, filters=None):
    return _lzma.decompress(data)


class LZMAFile:
    def __init__(self, filename=None, mode="r", *, format=None, check=-1,
                 preset=None, filters=None):
        if filename is None:
            raise TypeError("LZMAFile requires a filename")
        if "b" not in mode:
            mode = mode + "b"
        self.mode = mode
        self.name = filename
        self._preset = preset if preset is not None else PRESET_DEFAULT
        self._readable = "r" in mode
        self._writable = "w" in mode or "a" in mode or "x" in mode
        self._raw = _builtin_open(filename, mode)
        self._buffer = b""
        self._buffer_pos = 0
        self._write_buffer = []
        self._closed = False

    def read(self, size=-1):
        if not self._readable:
            raise OSError("not readable")
        if not self._buffer:
            raw = self._raw.read()
            if not raw:
                return b""
            self._buffer = decompress(raw)
            self._buffer_pos = 0
        if size is None or size < 0:
            chunk = self._buffer[self._buffer_pos:]
            self._buffer_pos = len(self._buffer)
            return chunk
        chunk = self._buffer[self._buffer_pos:self._buffer_pos + size]
        self._buffer_pos += len(chunk)
        return chunk

    def write(self, data):
        if not self._writable:
            raise OSError("not writable")
        if isinstance(data, str):
            data = data.encode("utf-8")
        self._write_buffer.append(data)
        return len(data)

    def flush(self):
        if self._write_buffer:
            payload = b"".join(self._write_buffer)
            self._raw.write(compress(payload, preset=self._preset))
            self._write_buffer = []

    def close(self):
        if self._closed:
            return
        self.flush()
        self._raw.close()
        self._closed = True

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.close()
        return False

    @property
    def closed(self):
        return self._closed

    def readable(self):
        return self._readable

    def writable(self):
        return self._writable

    def seekable(self):
        return False


def open(filename, mode="rb", *, format=None, check=-1, preset=None,
         filters=None, encoding=None, errors=None, newline=None):
    if "t" in mode:
        binary_mode = mode.replace("t", "b")
        if "b" not in binary_mode:
            binary_mode += "b"
        binary = LZMAFile(filename, binary_mode.replace("t", ""),
                          preset=preset)
        return io.TextIOWrapper(binary, encoding=encoding or "utf-8",
                                errors=errors or "strict",
                                newline=newline)
    return LZMAFile(filename, mode, preset=preset)


__all__ = ["compress", "decompress", "LZMAFile", "LZMAError", "open",
           "FORMAT_AUTO", "FORMAT_XZ", "FORMAT_ALONE", "FORMAT_RAW",
           "CHECK_NONE", "CHECK_CRC32", "CHECK_CRC64", "CHECK_SHA256",
           "PRESET_DEFAULT", "PRESET_EXTREME"]
