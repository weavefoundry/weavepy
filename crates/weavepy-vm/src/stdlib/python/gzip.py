"""Public ``gzip`` module (RFC 0019).

A thin wrapper over the Rust-backed ``_gzip`` core that exposes
``compress``, ``decompress``, ``open``, ``BadGzipFile``, and the
file-like ``GzipFile`` class CPython ships.
"""

import _gzip
import io
import os

_builtin_open = open

WRITE = "wb"
READ = "rb"
APPEND = "ab"


class BadGzipFile(OSError):
    """Raised when a gzip stream is malformed."""


def compress(data, compresslevel=9, *, mtime=None):
    if mtime is not None:
        # We accept the argument for API parity but currently ignore
        # the explicit mtime field; real CPython uses it to set the
        # gzip header timestamp.
        pass
    return _gzip.compress(data, compresslevel)


def decompress(data):
    try:
        return _gzip.decompress(data)
    except ValueError as e:
        raise BadGzipFile(str(e)) from None


class GzipFile:
    """File-like wrapper around a gzip stream."""

    def __init__(self, filename=None, mode=None, compresslevel=9,
                 fileobj=None, mtime=None):
        if mode is None:
            mode = "rb"
        if "b" not in mode:
            mode = mode + "b"
        self.mode = mode
        self.name = filename or (getattr(fileobj, "name", "<fdopen>"))
        self._level = compresslevel
        self.compresslevel = compresslevel
        self.mtime = mtime
        self._readable = "r" in mode
        self._writable = "w" in mode or "a" in mode or "x" in mode
        if fileobj is not None:
            self._raw = fileobj
            self._owns = False
        else:
            self._raw = _builtin_open(filename, mode)
            self._owns = True
        self._buffer = b""
        self._buffer_pos = 0
        self._write_buffer = []
        self._closed = False

    # ---- read path ----

    def read(self, size=-1):
        if not self._readable:
            raise OSError("not readable")
        # Lazy: read whole file, decompress, slice.
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

    def readline(self, size=-1):
        data = self.read(-1 if size is None or size < 0 else size)
        idx = data.find(b"\n")
        if idx >= 0:
            line = data[:idx + 1]
            self._buffer = data[idx + 1:] + self._buffer[self._buffer_pos:]
            self._buffer_pos = 0
            return line
        return data

    # ---- write path ----

    def write(self, data):
        if not self._writable:
            raise OSError("not writable")
        if isinstance(data, str):
            data = data.encode("utf-8")
        self._write_buffer.append(data)
        return len(data)

    def flush(self):
        # Best-effort: gzip has streaming framing but our core only
        # exposes "compress everything at once". On flush we
        # write the compressed payload to the raw file.
        if self._write_buffer:
            payload = b"".join(self._write_buffer)
            compressed = compress(payload, self._level)
            self._raw.write(compressed)
            self._write_buffer = []

    def close(self):
        if self._closed:
            return
        self.flush()
        if self._owns:
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
    """Open a gzip-compressed file, returning a binary or text mode wrapper."""
    if "t" in mode:
        binary_mode = mode.replace("t", "b")
        if "b" not in binary_mode:
            binary_mode += "b"
        binary = GzipFile(filename, binary_mode.replace("t", ""),
                          compresslevel=compresslevel)
        return io.TextIOWrapper(binary, encoding=encoding or "utf-8",
                                errors=errors or "strict",
                                newline=newline)
    return GzipFile(filename, mode, compresslevel=compresslevel)


__all__ = ["compress", "decompress", "GzipFile", "BadGzipFile", "open",
           "READ", "WRITE", "APPEND"]
