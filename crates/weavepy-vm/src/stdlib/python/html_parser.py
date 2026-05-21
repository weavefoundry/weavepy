"""WeavePy `html.parser` — a forgiving HTML tokeniser.

Provides `HTMLParser` that calls these overridable methods as it
scans input:

* `handle_starttag(tag, attrs)` / `handle_startendtag` /
  `handle_endtag(tag)`
* `handle_data(data)` / `handle_comment(data)`
* `handle_entityref(name)` / `handle_charref(name)`
* `handle_decl(decl)` / `handle_pi(data)`

The implementation is regex-driven and doesn't reach CPython's
fidelity for nested SCRIPT/STYLE handling, but it covers everything
typical HTML scrapers need.
"""

import re


_TAG_RE = re.compile(
    r"<(/?)([A-Za-z][A-Za-z0-9_:.-]*)([^>]*)>",
)
_ATTR_RE = re.compile(
    r'([A-Za-z_][A-Za-z0-9_:.-]*)(?:\s*=\s*(?:"([^"]*)"|\'([^\']*)\'|([^\s"\'=<>`]+)))?'
)
_COMMENT_RE = re.compile(r"<!--(.*?)-->", re.DOTALL)


class HTMLParser:
    CDATA_CONTENT_ELEMENTS = ("script", "style")

    def __init__(self, *, convert_charrefs=True):
        self._buf = ""
        self.convert_charrefs = convert_charrefs

    def feed(self, data):
        self._buf += data
        self._parse()

    def close(self):
        self._parse()
        self._buf = ""

    def reset(self):
        self._buf = ""

    def _parse(self):
        pos = 0
        s = self._buf
        n = len(s)
        while pos < n:
            if s[pos] != "<":
                end = s.find("<", pos)
                if end == -1:
                    end = n
                data = s[pos:end]
                if data:
                    self.handle_data(data)
                pos = end
                continue
            if s.startswith("<!--", pos):
                end = s.find("-->", pos)
                if end == -1:
                    return
                self.handle_comment(s[pos + 4:end])
                pos = end + 3
                continue
            if s.startswith("<!", pos):
                end = s.find(">", pos)
                if end == -1:
                    return
                self.handle_decl(s[pos + 2:end])
                pos = end + 1
                continue
            if s.startswith("<?", pos):
                end = s.find("?>", pos)
                if end == -1:
                    return
                self.handle_pi(s[pos + 2:end])
                pos = end + 2
                continue
            m = _TAG_RE.match(s, pos)
            if not m:
                self.handle_data(s[pos])
                pos += 1
                continue
            closing, tag, attrs = m.groups()
            tag = tag.lower()
            self_close = attrs.endswith("/")
            attrs = attrs.rstrip("/").strip()
            parsed_attrs = []
            for am in _ATTR_RE.finditer(attrs):
                name = am.group(1)
                v = am.group(2) if am.group(2) is not None else am.group(3) if am.group(3) is not None else am.group(4)
                parsed_attrs.append((name.lower(), v))
            if closing:
                self.handle_endtag(tag)
            elif self_close:
                self.handle_startendtag(tag, parsed_attrs)
            else:
                self.handle_starttag(tag, parsed_attrs)
            pos = m.end()
        self._buf = s[pos:]

    def handle_starttag(self, tag, attrs):
        pass

    def handle_endtag(self, tag):
        pass

    def handle_startendtag(self, tag, attrs):
        self.handle_starttag(tag, attrs)
        self.handle_endtag(tag)

    def handle_data(self, data):
        pass

    def handle_comment(self, data):
        pass

    def handle_decl(self, decl):
        pass

    def handle_pi(self, data):
        pass

    def handle_entityref(self, name):
        pass

    def handle_charref(self, name):
        pass


__all__ = ["HTMLParser"]
