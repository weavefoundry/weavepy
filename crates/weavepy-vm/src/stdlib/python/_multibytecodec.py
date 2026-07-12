"""WeavePy's `_multibytecodec` surface.

CPython implements the CJK codecs in C (`Modules/cjkcodecs/multibytecodec.c`)
and exposes these base classes; WeavePy implements the same codecs as frozen
Python modules (`_codec_cjk_dbcs`, `_codec_cjk_ext`, `_codec_euc_jis_2004`),
so the classes here exist for the module surface itself: the base types are
not usable directly — like CPython's, they require a ``codec`` attribute that
only properly-constructed subclasses provide (bug #3305: instantiating the
bare base type raises AttributeError, not a crash).
"""


class MultibyteCodec:
    """Opaque codec handle (CPython's `MultibyteCodec` type)."""

    def encode(self, input, errors=None):
        raise self._no_codec()

    def decode(self, input, errors=None):
        raise self._no_codec()

    @staticmethod
    def _no_codec():
        return TypeError("unbound MultibyteCodec object")


class MultibyteIncrementalEncoder:
    def __init__(self, errors="strict"):
        self.codec  # AttributeError on the bare base type, like CPython
        self.errors = errors


class MultibyteIncrementalDecoder:
    def __init__(self, errors="strict"):
        self.codec
        self.errors = errors


class MultibyteStreamReader:
    def __init__(self, stream, errors="strict"):
        self.codec
        self.stream = stream
        self.errors = errors


class MultibyteStreamWriter:
    def __init__(self, stream, errors="strict"):
        self.codec
        self.stream = stream
        self.errors = errors


def __create_codec(arg):
    raise TypeError("argument type invalid")
