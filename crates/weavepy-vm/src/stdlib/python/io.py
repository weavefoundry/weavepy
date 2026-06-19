"""The io module provides the Python interfaces to stream handling.

The builtin open function is defined in this module.

WeavePy note (RFC 0040 WS7)
---------------------------
This mirrors CPython's real ``Lib/io.py``: the public ``io`` namespace is a
thin wrapper that re-exports the C accelerator (``_io``). In particular
``io.BufferedReader is _io.BufferedReader``, ``type(open(f, 'rb')) is
io.BufferedReader``, and the ``IOBase`` ABC family is shared with ``_io`` — so
type identity, ``isinstance`` and the C/Py test split in ``test_io`` all behave
as on CPython. The pure-Python reference implementation lives separately in
``_pyio`` (imported directly by ``test_io`` as its "Py" variant), exactly as on
CPython; it does *not* shadow the native classes here.
"""

__author__ = ("Guido van Rossum <guido@python.org>, "
    "Mike Verdone <mike.verdone@gmail.com>, "
    "Mark Russell <mark.russell@zen.co.uk>, "
    "Antoine Pitrou <solipsis@pitrou.net>, "
    "Amaury Forgeot d'Arc <amauryfa@gmail.com>, "
    "Benjamin Peterson <benjamin@python.org>")

__all__ = ["BlockingIOError", "open", "open_code", "IOBase", "RawIOBase",
           "FileIO", "BytesIO", "StringIO", "BufferedIOBase",
           "BufferedReader", "BufferedWriter", "BufferedRWPair",
           "BufferedRandom", "TextIOBase", "TextIOWrapper",
           "UnsupportedOperation", "SEEK_SET", "SEEK_CUR", "SEEK_END",
           "DEFAULT_BUFFER_SIZE", "text_encoding", "IncrementalNewlineDecoder"]

# Exactly like CPython's `Lib/io.py`: the public `io` namespace re-exports the
# native accelerator (`_io`) wholesale, so `io.BufferedReader is
# _io.BufferedReader`, `type(open(f, 'rb')) is io.BufferedReader`, and the
# `IOBase` ABC family is shared. Every other stdlib module (`tarfile`,
# `zipfile`, `subprocess`, `gzip`, …) goes through this fast native stack — the
# pure-Python reference (`_pyio`) is *not* wired in here; `test_io` imports it
# directly to build its "Py" variant, matching CPython. (Routing the public
# `io` through `_pyio` would be both slower — pure-Python buffering over the
# `os.*` syscalls — and unfaithful to CPython's C-default architecture.)
from _io import (DEFAULT_BUFFER_SIZE, BlockingIOError, UnsupportedOperation,
                 open_code, open, FileIO, BytesIO, StringIO, IOBase, RawIOBase,
                 BufferedIOBase, BufferedReader, BufferedWriter, BufferedRWPair,
                 BufferedRandom, TextIOBase, TextIOWrapper, text_encoding,
                 IncrementalNewlineDecoder, SEEK_SET, SEEK_CUR, SEEK_END)
