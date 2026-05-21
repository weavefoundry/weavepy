"""WeavePy `email.generator` — serialise `Message` objects."""


class Generator:
    """Serialise a `Message` to a text stream."""

    def __init__(self, outfp, mangle_from_=True, maxheaderlen=78, *, policy=None):
        self._outfp = outfp
        self._mangle_from_ = mangle_from_
        self._maxheaderlen = maxheaderlen

    def flatten(self, msg, unixfrom=False, linesep="\n"):
        if unixfrom:
            self._outfp.write("From WeavePy " + linesep)
        for k, v in msg.items():
            self._outfp.write("{}: {}".format(k, v) + linesep)
        self._outfp.write(linesep)
        payload = msg.get_payload()
        if isinstance(payload, str):
            self._outfp.write(payload)
        elif isinstance(payload, list):
            boundary = "===WeavePyBoundary==="
            for p in payload:
                self._outfp.write("--{}{}".format(boundary, linesep))
                Generator(self._outfp).flatten(p, linesep=linesep)
            self._outfp.write("--{}--{}".format(boundary, linesep))


class BytesGenerator(Generator):
    """Like `Generator` but `flatten()` outputs bytes."""

    def flatten(self, msg, unixfrom=False, linesep=b"\n"):
        class _Wrap:
            def __init__(self, parent):
                self._parent = parent

            def write(self, data):
                if isinstance(data, str):
                    data = data.encode("utf-8")
                self._parent.write(data)
        sep = linesep.decode("ascii") if isinstance(linesep, (bytes, bytearray)) else linesep
        Generator(_Wrap(self._outfp)).flatten(msg, unixfrom=unixfrom, linesep=sep)


__all__ = ["Generator", "BytesGenerator"]
