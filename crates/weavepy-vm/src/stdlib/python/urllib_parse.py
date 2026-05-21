"""WeavePy `urllib.parse` — URL parsing.

Implements `urlparse`, `urlsplit`, `urljoin`, `urldefrag`,
`urlencode`, `urlunparse`, `quote`, `quote_plus`, `unquote`,
`unquote_plus`, `quote_from_bytes`, `unquote_to_bytes`, `parse_qs`,
`parse_qsl`, plus the named-tuple-shaped `ParseResult` /
`SplitResult` return types.

The implementation matches CPython for the common subset; the
internal `_uses_relative`, `_uses_netloc`, `_uses_params` sets that
CPython uses for scheme-specific behaviour are reproduced.
"""


_HEXDIG = "0123456789ABCDEFabcdef"
_ALWAYS_SAFE = frozenset(
    "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_.-~"
)

_uses_relative = frozenset([
    "ftp", "http", "https", "gopher", "nntp", "imap", "wais", "file",
    "mms", "shttp", "mmst", "mmsu", "prospero", "rtsp", "rtspu", "sftp",
    "svn", "svn+ssh", "ws", "wss",
])

_uses_netloc = frozenset([
    "ftp", "http", "https", "gopher", "nntp", "telnet", "imap", "wais",
    "file", "mms", "shttp", "mmst", "mmsu", "prospero", "rtsp", "rtspu",
    "sftp", "svn", "svn+ssh", "ws", "wss", "ssh",
])

_uses_params = frozenset([
    "ftp", "hdl", "prospero", "http", "imap", "https", "shttp", "rtsp",
    "rtspu", "sip", "sips", "mms", "sftp", "tel",
])


class SplitResult:
    """A 5-tuple-like (scheme, netloc, path, query, fragment).

    CPython subclasses `tuple`; WeavePy doesn't (yet) support tuple
    subclassing with `__new__`, so we implement the `tuple` surface
    we care about (`__getitem__`, `__iter__`, `__len__`, `geturl`)
    directly.
    """

    _fields = ("scheme", "netloc", "path", "query", "fragment")

    def __init__(self, scheme, netloc, path, query, fragment):
        self.scheme = scheme
        self.netloc = netloc
        self.path = path
        self.query = query
        self.fragment = fragment

    def __getitem__(self, idx):
        return (self.scheme, self.netloc, self.path, self.query, self.fragment)[idx]

    def __iter__(self):
        return iter((self.scheme, self.netloc, self.path, self.query, self.fragment))

    def __len__(self):
        return 5

    def __eq__(self, other):
        try:
            return tuple(self) == tuple(other)
        except TypeError:
            return False

    def __repr__(self):
        return "SplitResult(scheme={!r}, netloc={!r}, path={!r}, query={!r}, fragment={!r})".format(
            self.scheme, self.netloc, self.path, self.query, self.fragment
        )

    def geturl(self):
        return urlunsplit(self)

    @property
    def hostname(self):
        return _parse_hostname(self.netloc)

    @property
    def port(self):
        return _parse_port(self.netloc)

    @property
    def username(self):
        return _parse_userinfo(self.netloc)[0]

    @property
    def password(self):
        return _parse_userinfo(self.netloc)[1]


class ParseResult:
    """A 6-tuple-like (scheme, netloc, path, params, query, fragment)."""

    _fields = ("scheme", "netloc", "path", "params", "query", "fragment")

    def __init__(self, scheme, netloc, path, params, query, fragment):
        self.scheme = scheme
        self.netloc = netloc
        self.path = path
        self.params = params
        self.query = query
        self.fragment = fragment

    def __getitem__(self, idx):
        return (self.scheme, self.netloc, self.path, self.params, self.query, self.fragment)[idx]

    def __iter__(self):
        return iter((self.scheme, self.netloc, self.path, self.params, self.query, self.fragment))

    def __len__(self):
        return 6

    def __eq__(self, other):
        try:
            return tuple(self) == tuple(other)
        except TypeError:
            return False

    def __repr__(self):
        return "ParseResult(scheme={!r}, netloc={!r}, path={!r}, params={!r}, query={!r}, fragment={!r})".format(
            self.scheme, self.netloc, self.path, self.params, self.query, self.fragment
        )

    def geturl(self):
        return urlunparse(self)

    @property
    def hostname(self):
        return _parse_hostname(self.netloc)

    @property
    def port(self):
        return _parse_port(self.netloc)

    @property
    def username(self):
        return _parse_userinfo(self.netloc)[0]

    @property
    def password(self):
        return _parse_userinfo(self.netloc)[1]


def _split_netloc(netloc):
    return netloc


def _parse_userinfo(netloc):
    if "@" in netloc:
        userinfo = netloc.split("@", 1)[0]
        if ":" in userinfo:
            user, pwd = userinfo.split(":", 1)
            return (user or None, pwd or None)
        return (userinfo or None, None)
    return (None, None)


def _parse_hostname(netloc):
    if "@" in netloc:
        netloc = netloc.split("@", 1)[1]
    if netloc.startswith("["):
        end = netloc.find("]")
        if end != -1:
            return netloc[1:end].lower()
    if ":" in netloc:
        return netloc.split(":", 1)[0].lower()
    return netloc.lower() if netloc else None


def _parse_port(netloc):
    if "@" in netloc:
        netloc = netloc.split("@", 1)[1]
    if netloc.startswith("["):
        end = netloc.find("]")
        if end != -1 and end + 1 < len(netloc) and netloc[end + 1] == ":":
            try:
                return int(netloc[end + 2:])
            except ValueError:
                return None
        return None
    if ":" in netloc:
        try:
            return int(netloc.rsplit(":", 1)[1])
        except ValueError:
            return None
    return None


def urlsplit(url, scheme="", allow_fragments=True):
    """Split a URL into a 5-tuple SplitResult."""
    if isinstance(url, bytes):
        url = url.decode("ascii")
    fragment = ""
    query = ""
    # Extract scheme.
    i = url.find(":")
    if i > 0 and url[:i].replace("+", "").replace("-", "").replace(".", "").isalnum() \
            and url[:i][0].isalpha():
        the_scheme = url[:i].lower()
        rest = url[i + 1:]
    else:
        the_scheme = scheme
        rest = url
    netloc = ""
    if rest.startswith("//"):
        netloc_end = len(rest)
        for c in "/?#":
            idx = rest.find(c, 2)
            if idx != -1 and idx < netloc_end:
                netloc_end = idx
        netloc = rest[2:netloc_end]
        rest = rest[netloc_end:]
    if allow_fragments and "#" in rest:
        rest, fragment = rest.split("#", 1)
    if "?" in rest:
        rest, query = rest.split("?", 1)
    return SplitResult(the_scheme, netloc, rest, query, fragment)


def urlparse(url, scheme="", allow_fragments=True):
    """Split a URL into a 6-tuple ParseResult (path-params split)."""
    split = urlsplit(url, scheme, allow_fragments)
    path = split.path
    params = ""
    if split.scheme in _uses_params and ";" in path:
        path, params = path.split(";", 1)
    return ParseResult(split.scheme, split.netloc, path, params, split.query, split.fragment)


def urlunparse(parts):
    parts = tuple(parts)
    if len(parts) == 6:
        scheme, netloc, path, params, query, fragment = parts
    elif len(parts) == 5:
        scheme, netloc, path, query, fragment = parts
        params = ""
    else:
        raise ValueError("urlunparse: tuple of 5 or 6 expected")
    if params:
        path = "{};{}".format(path, params)
    return urlunsplit((scheme, netloc, path, query, fragment))


def urlunsplit(parts):
    scheme, netloc, path, query, fragment = parts
    url = ""
    if netloc or (scheme and scheme in _uses_netloc and not url.startswith("//")):
        if path and not path.startswith("/"):
            path = "/" + path
        url = "//" + netloc + path
    else:
        url = path
    if scheme:
        url = scheme + ":" + url
    if query:
        url = url + "?" + query
    if fragment:
        url = url + "#" + fragment
    return url


def urljoin(base, url, allow_fragments=True):
    if not base:
        return url
    if not url:
        return base
    bscheme, bnetloc, bpath, bparams, bquery, bfragment = urlparse(base, "", allow_fragments)
    scheme, netloc, path, params, query, fragment = urlparse(url, bscheme, allow_fragments)
    if scheme != bscheme or scheme not in _uses_relative:
        return url
    if scheme in _uses_netloc and netloc:
        return urlunparse((scheme, netloc, path, params, query, fragment))
    netloc = bnetloc
    if not path and not params:
        path = bpath
        params = bparams
        if not query:
            query = bquery
        return urlunparse((scheme, netloc, path, params, query, fragment))
    if path.startswith("/"):
        return urlunparse((scheme, netloc, path, params, query, fragment))
    segments = bpath.split("/")[:-1] + path.split("/")
    resolved = []
    for seg in segments:
        if seg == ".":
            continue
        if seg == "..":
            if resolved:
                resolved.pop()
            continue
        resolved.append(seg)
    if resolved and resolved[-1] != "":
        # Preserve trailing slash semantics.
        pass
    return urlunparse((scheme, netloc, "/".join(resolved) or "/", params, query, fragment))


def urldefrag(url):
    """Strip the fragment from `url`, returning `(defragged, fragment)`."""
    if "#" in url:
        s, frag = url.split("#", 1)
        return DefragResult(s, frag)
    return DefragResult(url, "")


class DefragResult:
    """A 2-tuple-like (url, fragment)."""

    _fields = ("url", "fragment")

    def __init__(self, url, fragment):
        self.url = url
        self.fragment = fragment

    def __getitem__(self, idx):
        return (self.url, self.fragment)[idx]

    def __iter__(self):
        return iter((self.url, self.fragment))

    def __len__(self):
        return 2

    def __eq__(self, other):
        try:
            return tuple(self) == tuple(other)
        except TypeError:
            return False

    def __repr__(self):
        return "DefragResult(url={!r}, fragment={!r})".format(self.url, self.fragment)

    def geturl(self):
        if self.fragment:
            return self.url + "#" + self.fragment
        return self.url


# ---- percent-encoding ---------------------------------------------


def quote(string, safe="/", encoding=None, errors=None):
    """Percent-encode `string` for inclusion in a URL."""
    if isinstance(string, str):
        data = string.encode(encoding or "utf-8", errors or "strict")
    else:
        data = bytes(string)
    return quote_from_bytes(data, safe)


def quote_plus(string, safe="", encoding=None, errors=None):
    if " " not in string:
        return quote(string, safe, encoding, errors)
    return quote(string, safe + " ", encoding, errors).replace(" ", "+")


def quote_from_bytes(data, safe="/"):
    if isinstance(safe, str):
        safe_bytes = safe.encode("ascii")
    else:
        safe_bytes = bytes(safe)
    safe_set = set(_ALWAYS_SAFE)
    for b in safe_bytes:
        safe_set.add(chr(b))
    out = []
    for byte in data:
        ch = chr(byte)
        if ch in safe_set:
            out.append(ch)
        else:
            out.append("%{:02X}".format(byte))
    return "".join(out)


def unquote_to_bytes(string):
    if isinstance(string, str):
        data = string.encode("ascii", "replace")
    else:
        data = bytes(string)
    res = bytearray()
    i = 0
    while i < len(data):
        if data[i] == 0x25 and i + 2 < len(data) and chr(data[i + 1]) in _HEXDIG and chr(data[i + 2]) in _HEXDIG:
            res.append(int(chr(data[i + 1]) + chr(data[i + 2]), 16))
            i += 3
        else:
            res.append(data[i])
            i += 1
    return bytes(res)


def unquote(string, encoding="utf-8", errors="replace"):
    return unquote_to_bytes(string).decode(encoding, errors)


def unquote_plus(string, encoding="utf-8", errors="replace"):
    return unquote(string.replace("+", " "), encoding, errors)


# ---- query string parsing -----------------------------------------


def urlencode(query, doseq=False, safe="", encoding=None, errors=None, quote_via=quote_plus):
    """Encode `query` (dict or list of tuples) as `a=1&b=2`."""
    if isinstance(query, dict):
        pairs = list(query.items())
    else:
        pairs = list(query)
    parts = []
    for key, value in pairs:
        k = quote_via(str(key), safe, encoding, errors)
        if doseq and isinstance(value, (list, tuple)):
            for v in value:
                parts.append("{}={}".format(k, quote_via(str(v), safe, encoding, errors)))
        else:
            parts.append("{}={}".format(k, quote_via(str(value), safe, encoding, errors)))
    return "&".join(parts)


def parse_qsl(qs, keep_blank_values=False, strict_parsing=False, encoding="utf-8", errors="replace",
              max_num_fields=None, separator="&"):
    if not qs:
        return []
    pairs = qs.split(separator)
    out = []
    for pair in pairs:
        if not pair and not keep_blank_values:
            continue
        if "=" in pair:
            k, v = pair.split("=", 1)
            k = unquote_plus(k, encoding, errors)
            v = unquote_plus(v, encoding, errors)
        else:
            k = unquote_plus(pair, encoding, errors)
            v = ""
        if v == "" and not keep_blank_values:
            continue
        out.append((k, v))
    return out


def parse_qs(qs, keep_blank_values=False, strict_parsing=False, encoding="utf-8", errors="replace",
             max_num_fields=None, separator="&"):
    out = {}
    for k, v in parse_qsl(qs, keep_blank_values, strict_parsing, encoding, errors,
                          max_num_fields, separator):
        out.setdefault(k, []).append(v)
    return out


__all__ = [
    "urlparse", "urlunparse", "urlsplit", "urlunsplit", "urljoin",
    "urldefrag", "urlencode", "parse_qs", "parse_qsl",
    "quote", "quote_plus", "unquote", "unquote_plus",
    "quote_from_bytes", "unquote_to_bytes",
    "ParseResult", "SplitResult", "DefragResult",
]
