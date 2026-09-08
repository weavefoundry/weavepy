"""WeavePy's `_multibytecodec` surface.

CPython implements the CJK codecs in C (`Modules/cjkcodecs/multibytecodec.c`)
and exposes these base classes; the `encodings.big5`-style stdlib modules
build their codec classes on them around a `codec` handle obtained from
`_codecs_tw.getcodec('big5')` &co. WeavePy implements the codecs themselves as
frozen Python modules (`_codec_cjk_dbcs`, `_codec_cjk_ext`,
`_codec_euc_jis_2004`), so a `MultibyteCodec` here is a handle onto one of
those `CodecInfo`s and the four base classes delegate to its incremental and
stream classes. Like CPython's, the bare base types are not usable directly:
they require a ``codec`` attribute that only properly-constructed subclasses
provide (bug #3305: instantiating the bare base type raises AttributeError,
not a crash).
"""


class MultibyteCodec:
    """Opaque codec handle (CPython's `MultibyteCodec` type)."""

    __slots__ = ("_info",)

    def __new__(cls, *args, **kwargs):
        raise TypeError("cannot create '_multibytecodec.MultibyteCodec' instances")

    @classmethod
    def _bind(cls, info):
        self = object.__new__(cls)
        self._info = info
        return self

    @property
    def name(self):
        return self._info.name

    def encode(self, input, errors=None):
        if errors is None:
            errors = "strict"
        elif not isinstance(errors, str):
            raise TypeError("encode() argument 'errors' must be str or None, "
                            "not %s" % type(errors).__name__)
        return self._info.encode(input, errors)

    def decode(self, input, errors=None):
        if errors is None:
            errors = "strict"
        elif not isinstance(errors, str):
            raise TypeError("decode() argument 'errors' must be str or None, "
                            "not %s" % type(errors).__name__)
        return self._info.decode(input, errors)



def _check_errors(errors):
    if not isinstance(errors, str):
        raise TypeError("argument 'errors' must be str, not %s"
                        % type(errors).__name__)
    return errors


class _Errors:
    """CPython's `errors` getset: a str, validated on assignment."""

    @property
    def errors(self):
        return self._impl.errors

    @errors.setter
    def errors(self, value):
        self._impl.errors = _check_errors(value)


class MultibyteIncrementalEncoder(_Errors):
    def __init__(self, errors="strict"):
        codec = self.codec  # AttributeError on the bare base type, like CPython
        self._impl = codec._info.incrementalencoder(_check_errors(errors))

    def encode(self, input, final=False):
        return self._impl.encode(input, final)

    def getstate(self):
        return self._impl.getstate()

    def setstate(self, state):
        return self._impl.setstate(state)

    def reset(self):
        return self._impl.reset()


class MultibyteIncrementalDecoder(_Errors):
    def __init__(self, errors="strict"):
        codec = self.codec
        self._impl = codec._info.incrementaldecoder(_check_errors(errors))

    def decode(self, input, final=False):
        return self._impl.decode(input, final)

    def getstate(self):
        return self._impl.getstate()

    def setstate(self, state):
        return self._impl.setstate(state)

    def reset(self):
        return self._impl.reset()


class MultibyteStreamReader(_Errors):
    def __init__(self, stream, errors="strict"):
        codec = self.codec
        self.stream = stream
        self._impl = codec._info.streamreader(stream, _check_errors(errors))

    def read(self, size=None):
        if size is None:
            size = -1
        return self._impl.read(size)

    def readline(self, size=None):
        return self._impl.readline(size)

    def readlines(self, sizehint=None):
        return self._impl.readlines(sizehint)

    def reset(self):
        return self._impl.reset()


class MultibyteStreamWriter(_Errors):
    def __init__(self, stream, errors="strict"):
        codec = self.codec
        self.stream = stream
        self._impl = codec._info.streamwriter(stream, _check_errors(errors))

    def write(self, object):
        return self._impl.write(object)

    def writelines(self, list):
        return self._impl.writelines(list)

    def reset(self):
        return self._impl.reset()


def __create_codec(arg):
    raise TypeError("argument type invalid")


# CPython does not export the handle type from `_multibytecodec`; the
# `_codecs_*` modules reach it through the private name.
_MultibyteCodec = MultibyteCodec
del MultibyteCodec
