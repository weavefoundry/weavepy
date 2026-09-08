"""WeavePy's `_codecs_jp` surface (CPython's `Modules/cjkcodecs/_codecs_jp.c`).

`getcodec(name)` hands the stdlib `encodings.*` modules a `MultibyteCodec`
handle onto the frozen Python implementation of the same codec, so those
modules load verbatim on top of `_multibytecodec`'s base classes.
"""

import codecs as _codecs
from _multibytecodec import _MultibyteCodec

_NAMES = frozenset(('shift_jis', 'cp932', 'euc_jp', 'shift_jis_2004', 'shift_jisx0213', 'euc_jis_2004', 'euc_jisx0213'))


def getcodec(name):
    if not isinstance(name, str):
        raise TypeError("encoding name must be a string.")
    if name not in _NAMES:
        raise LookupError("no such codec is supported.")
    return _MultibyteCodec._bind(_codecs.lookup(name))
