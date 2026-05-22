"""Public ``bz2`` module (RFC 0019).

Thin wrapper over the Rust-backed ``_bz2`` core that exposes
``compress``, ``decompress``, ``open``, and ``BZ2File``.
"""

import _bz2
import io

_builtin_open = open


def compress(data, compresslevel=9):
    return _bz2.compress(data, compresslevel)


def decompress(data):
    return _bz2.decompress(data)


class BZ2File:
    """File-like wrapper around a bzip2 stream."""

    def __init__(self, filename, mode="rb", *, compresslevel=9):
        if "b" not in mode:
            mode = mode + "b"
        self.mode = mode
        self.name = filename
        self._level = compresslevel
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
            self._raw.write(compress(payload, self._level))
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


def open(filename, mode="rb", compresslevel=9, encoding=None,
         errors=None, newline=None):
    if "t" in mode:
        binary_mode = mode.replace("t", "b")
        if "b" not in binary_mode:
            binary_mode += "b"
        binary = BZ2File(filename, binary_mode.replace("t", ""),
                         compresslevel=compresslevel)
        return io.TextIOWrapper(binary, encoding=encoding or "utf-8",
                                errors=errors or "strict",
                                newline=newline)
    return BZ2File(filename, mode, compresslevel=compresslevel)


__all__ = ["compress", "decompress", "BZ2File", "open"]
