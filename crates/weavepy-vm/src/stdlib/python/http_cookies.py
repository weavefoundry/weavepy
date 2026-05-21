"""WeavePy `http.cookies` — RFC 2109 / RFC 6265 cookie jar.

The shape mirrors CPython's `http.cookies` enough for typical code:
`Morsel`-like objects with `key`, `value`, attributes; a
`BaseCookie` mapping of name → morsel with `output`/`load`.

We intentionally do *not* subclass `dict` (WeavePy's `dict` subclass
support is limited); both `Morsel` and `BaseCookie` provide the
mapping protocol via `__getitem__` / `__setitem__` /
`__iter__` / `keys` / `items` / `values`.
"""


__all__ = ["CookieError", "Morsel", "BaseCookie", "SimpleCookie"]


_RESERVED = {
    "expires": "expires",
    "path": "Path",
    "comment": "Comment",
    "domain": "Domain",
    "max-age": "Max-Age",
    "secure": "Secure",
    "httponly": "HttpOnly",
    "version": "Version",
    "samesite": "SameSite",
}


_FLAGS = {"secure", "httponly"}


class CookieError(Exception):
    pass


class Morsel:
    """A single cookie key/value with optional attributes."""

    def __init__(self):
        self._attrs = {k: "" for k in _RESERVED}
        self.key = None
        self.value = None
        self.coded_value = None

    def set(self, key, value, coded_value):
        if key.lower() in _RESERVED:
            raise CookieError("reserved key: {}".format(key))
        self.key = key
        self.value = value
        self.coded_value = coded_value

    def __getitem__(self, name):
        if name not in self._attrs:
            raise KeyError(name)
        return self._attrs[name]

    def __setitem__(self, name, value):
        if name.lower() not in _RESERVED:
            raise CookieError("invalid attribute {}".format(name))
        self._attrs[name.lower()] = value

    def __contains__(self, name):
        return name in self._attrs

    def __iter__(self):
        return iter(self._attrs)

    def keys(self):
        return list(self._attrs.keys())

    def values(self):
        return list(self._attrs.values())

    def items(self):
        return list(self._attrs.items())

    def get(self, name, default=None):
        return self._attrs.get(name, default)

    def OutputString(self, attrs=None):
        result = ["{}={}".format(self.key, self.coded_value)]
        items = self.items() if attrs is None else [(k, self[k]) for k in attrs]
        for k, v in items:
            if not v:
                continue
            if k.lower() in _FLAGS:
                result.append(_RESERVED[k.lower()])
            else:
                result.append("{}={}".format(_RESERVED.get(k.lower(), k), v))
        return "; ".join(result)

    def __str__(self):
        return self.OutputString()

    def __repr__(self):
        return "<Morsel {}={}>".format(self.key, self.coded_value)


class BaseCookie:
    """Mapping of cookie name → :class:`Morsel`."""

    def __init__(self, input=None):
        self._morsels = {}
        if input is not None:
            self.load(input)

    def value_decode(self, val):
        return val, val

    def value_encode(self, val):
        s = str(val)
        return s, s

    def load(self, rawdata):
        if isinstance(rawdata, str):
            self._parse(rawdata)
        elif isinstance(rawdata, dict):
            for k, v in rawdata.items():
                self[k] = v

    def _parse(self, s):
        for part in s.split(";"):
            part = part.strip()
            if not part:
                continue
            if "=" in part:
                k, _, v = part.partition("=")
                k = k.strip()
                v = v.strip().strip('"')
                if k.lower() in _RESERVED:
                    continue
                value, coded_value = self.value_decode(v)
                m = Morsel()
                m.set(k, value, coded_value)
                self._morsels[k] = m

    def __getitem__(self, key):
        return self._morsels[key]

    def __setitem__(self, key, value):
        if isinstance(value, Morsel):
            self._morsels[key] = value
            return
        v, coded = self.value_encode(value)
        m = Morsel()
        m.set(key, v, coded)
        self._morsels[key] = m

    def __delitem__(self, key):
        del self._morsels[key]

    def __contains__(self, key):
        return key in self._morsels

    def __iter__(self):
        return iter(self._morsels)

    def __len__(self):
        return len(self._morsels)

    def keys(self):
        return list(self._morsels.keys())

    def values(self):
        return list(self._morsels.values())

    def items(self):
        return list(self._morsels.items())

    def get(self, key, default=None):
        return self._morsels.get(key, default)

    def output(self, attrs=None, header="Set-Cookie:", sep="\r\n"):
        return sep.join("{} {}".format(header, m.OutputString(attrs)) for m in self.values())

    def __str__(self):
        return self.output()

    def __repr__(self):
        return "<{} {!r}>".format(type(self).__name__, list(self.keys()))


class SimpleCookie(BaseCookie):
    """Concrete `BaseCookie` with identity value encode/decode."""
