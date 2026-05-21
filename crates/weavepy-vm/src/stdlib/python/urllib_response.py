"""WeavePy `urllib.response` — addinfourl helpers.

CPython exposes `addinfourl(fp, headers, url, code)` for legacy
compatibility. We reproduce just the surface modern code uses.
"""


class addbase:
    """Base wrapper around a file-like object."""

    def __init__(self, fp):
        self.fp = fp

    def close(self):
        if self.fp is not None:
            try:
                self.fp.close()
            except Exception:
                pass
            self.fp = None

    def read(self, *a, **kw):
        return self.fp.read(*a, **kw)

    def readline(self):
        return self.fp.readline()

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.close()
        return False


class addinfo(addbase):
    """`addbase` + an `info()` method returning a headers mapping."""

    def __init__(self, fp, headers):
        addbase.__init__(self, fp)
        self.headers = headers

    def info(self):
        return self.headers


class addinfourl(addinfo):
    """`addinfo` + a `geturl()` method exposing the actual URL."""

    def __init__(self, fp, headers, url, code=None):
        addinfo.__init__(self, fp, headers)
        self.url = url
        self.code = code

    def geturl(self):
        return self.url

    def getcode(self):
        return self.code


__all__ = ["addbase", "addinfo", "addinfourl"]
