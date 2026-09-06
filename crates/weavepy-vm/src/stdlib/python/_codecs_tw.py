"""WeavePy's `_codecs_tw` surface (CPython's `Modules/cjkcodecs/_codecs_tw.c`).

`getcodec(name)` hands the stdlib `encodings.*` modules a `MultibyteCodec`
handle onto the frozen Python implementation of the same codec, so those
modules load verbatim on top of `_multibytecodec`'s base classes.
"""

import codecs as _codecs
from _multibytecodec import _MultibyteCodec

_NAMES = frozenset(('big5', 'cp950'))


def getcodec(name):
    if not isinstance(name, str):
        raise TypeError("encoding name must be a string.")
    if name not in _NAMES:
        raise LookupError("no such codec is supported.")
    return _MultibyteCodec._bind(_codecs.lookup(name))
