"""The io module provides the Python interfaces to stream handling.

The builtin open function is defined in this module.

WeavePy note (RFC 0040 WS7)
---------------------------
This mirrors CPython's real ``Lib/io.py``: the public ``io`` namespace re-exports
the C accelerator's (``_io``) *concrete* classes wholesale — so
``io.BufferedReader is _io.BufferedReader`` and ``type(open(f, 'rb')) is
io.BufferedReader`` — while the four *abstract base classes* (``IOBase``,
``RawIOBase``, ``BufferedIOBase``, ``TextIOBase``) are re-declared here on top of
their native bases with an ``abc.ABCMeta`` metaclass, exactly as CPython does
(its comment: "Declaring ABCs in C is tricky so we do it here"). The native
concrete streams are wired in as virtual subclasses via ``register()``, so
``isinstance``/``issubclass`` and ``test_io``'s ``test_abcs`` (which asserts
``isinstance(io.IOBase, abc.ABCMeta)``) behave as on CPython. The pure-Python
reference implementation lives separately in ``_pyio`` (imported directly by
``test_io`` as its "Py" variant), exactly as on CPython; it does *not* shadow
the native classes here, and registers its own classes against these same ABCs.
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
# native accelerator's *concrete* classes (`_io`) wholesale — so
# `io.BufferedReader is _io.BufferedReader`, `type(open(f, 'rb')) is
# io.BufferedReader` — while the four *abstract base classes* are (re)declared
# here on top of their native bases with an `abc.ABCMeta` metaclass, just as
# CPython does in `Lib/io.py`. Declaring the ABCs in native code is awkward
# (CPython's comment: "Declaring ABCs in C is tricky so we do it here"), so the
# `metaclass=abc.ABCMeta` identity that `test_io`'s `test_abcs` asserts
# (`isinstance(io.IOBase, abc.ABCMeta)`) is established at the Python layer,
# with the concrete native streams wired in as virtual subclasses via
# `register()`. Method descriptions and default implementations are inherited
# from the native base. The pure-Python reference (`_pyio`) is *not* wired in
# here; `test_io` imports it directly to build its "Py" variant, matching
# CPython.
import _io
import abc

from _io import (DEFAULT_BUFFER_SIZE, BlockingIOError, UnsupportedOperation,
                 open_code, open, FileIO, BytesIO, StringIO, BufferedReader,
                 BufferedWriter, BufferedRWPair, BufferedRandom, TextIOWrapper,
                 text_encoding, IncrementalNewlineDecoder,
                 SEEK_SET, SEEK_CUR, SEEK_END)

# `UnsupportedOperation` is already reported as living in `io` by the native
# accelerator (its `__module__` is "io" and immutable), so — unlike CPython,
# which patches it here — nothing more is needed.

# Declaring ABCs in native code is tricky so we do it here, exactly like
# CPython's `Lib/io.py`. Method descriptions and default implementations are
# inherited from the native (`_io`) version, but the `abc.ABCMeta` metaclass —
# and therefore `register()` for virtual-subclass declaration — is established
# at this Python layer.
class IOBase(_io.IOBase, metaclass=abc.ABCMeta):
    __doc__ = _io.IOBase.__doc__

class RawIOBase(_io.RawIOBase, IOBase):
    __doc__ = _io.RawIOBase.__doc__

class BufferedIOBase(_io.BufferedIOBase, IOBase):
    __doc__ = _io.BufferedIOBase.__doc__

class TextIOBase(_io.TextIOBase, IOBase):
    __doc__ = _io.TextIOBase.__doc__

RawIOBase.register(FileIO)

for klass in (BytesIO, BufferedReader, BufferedWriter, BufferedRandom,
              BufferedRWPair):
    BufferedIOBase.register(klass)

for klass in (StringIO, TextIOWrapper):
    TextIOBase.register(klass)
del klass
