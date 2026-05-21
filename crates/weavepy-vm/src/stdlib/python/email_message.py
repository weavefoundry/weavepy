"""WeavePy `email.message` — a tiny RFC 5322 Message object.

CPython's `email` package is vast. This implementation covers the
parts most code touches: parsing a single MIME message, walking
headers, getting/setting payloads. Multipart traversal is supported
shallowly.
"""


class Message:
    """A single email message."""

    def __init__(self):
        self._headers = []
        self._payload = None
        self.preamble = None
        self.epilogue = None
        self.defects = []

    def __getitem__(self, name):
        for k, v in self._headers:
            if k.lower() == name.lower():
                return v
        return None

    def __setitem__(self, name, value):
        self._headers.append((name, value))

    def __delitem__(self, name):
        self._headers = [(k, v) for k, v in self._headers if k.lower() != name.lower()]

    def __contains__(self, name):
        return any(k.lower() == name.lower() for k, _ in self._headers)

    def __iter__(self):
        for k, _ in self._headers:
            yield k

    def __len__(self):
        return len(self._headers)

    def keys(self):
        return [k for k, _ in self._headers]

    def values(self):
        return [v for _, v in self._headers]

    def items(self):
        return list(self._headers)

    def get(self, name, failobj=None):
        v = self[name]
        return v if v is not None else failobj

    def get_all(self, name, failobj=None):
        out = [v for k, v in self._headers if k.lower() == name.lower()]
        return out if out else failobj

    def add_header(self, _name, _value, **params):
        parts = [_value]
        for k, v in params.items():
            if v is None:
                parts.append(k.replace("_", "-"))
            else:
                parts.append('{}="{}"'.format(k.replace("_", "-"), v))
        self._headers.append((_name, "; ".join(parts)))

    def replace_header(self, name, value):
        for i, (k, _) in enumerate(self._headers):
            if k.lower() == name.lower():
                self._headers[i] = (k, value)
                return
        raise KeyError(name)

    def get_content_type(self):
        ct = self["Content-Type"]
        if ct is None:
            return "text/plain"
        return ct.split(";", 1)[0].strip().lower()

    def get_content_maintype(self):
        return self.get_content_type().split("/")[0]

    def get_content_subtype(self):
        return self.get_content_type().split("/")[1] if "/" in self.get_content_type() else ""

    def get_default_type(self):
        return "text/plain"

    def set_default_type(self, ctype):
        pass

    def get_payload(self, i=None, decode=False):
        if i is None:
            return self._payload
        if not isinstance(self._payload, list):
            raise TypeError("not a multipart message")
        return self._payload[i]

    def set_payload(self, payload, charset=None):
        self._payload = payload
        if charset is not None:
            self.set_charset(charset)

    def set_charset(self, charset):
        del self["Content-Type"]
        self["Content-Type"] = "text/plain; charset={}".format(charset)

    def get_charset(self):
        ct = self["Content-Type"]
        if ct is None:
            return None
        for part in ct.split(";")[1:]:
            part = part.strip()
            if part.lower().startswith("charset="):
                return part.split("=", 1)[1].strip().strip('"')
        return None

    def is_multipart(self):
        return isinstance(self._payload, list)

    def walk(self):
        yield self
        if self.is_multipart():
            for sub in self._payload:
                for s in sub.walk():
                    yield s

    def as_string(self, unixfrom=False, maxheaderlen=0, policy=None):
        from email.generator import Generator
        out = []
        for k, v in self._headers:
            out.append("{}: {}".format(k, v))
        out.append("")
        if isinstance(self._payload, str):
            out.append(self._payload)
        elif isinstance(self._payload, list):
            boundary = "===WeavePyBoundary==="
            for p in self._payload:
                out.append("--" + boundary)
                out.append(p.as_string())
            out.append("--" + boundary + "--")
        return "\n".join(out)

    def __str__(self):
        return self.as_string()


class EmailMessage(Message):
    """RFC 5322 message + the small slice of the EmailMessage API
    most code touches (`set_content`, `add_attachment`, etc.)."""

    def set_content(self, content=None, *args, **kw):
        # The CPython API has a much richer signature; here we just
        # handle the typical `set_content("text/plain string")` case.
        subtype = kw.get("subtype", "plain")
        charset = kw.get("charset", "utf-8")
        if "Content-Type" not in self:
            self["Content-Type"] = "text/{}; charset={}".format(subtype, charset)
        if "Content-Transfer-Encoding" not in self:
            self["Content-Transfer-Encoding"] = "7bit"
        if "MIME-Version" not in self:
            self["MIME-Version"] = "1.0"
        if isinstance(content, bytes):
            content = content.decode(charset)
        self.set_payload(content if content is not None else "")

    def add_attachment(self, content=None, **kw):
        # Minimal attachment support — just appends a sub-message.
        if not isinstance(self._payload, list):
            existing = self._payload
            self._payload = []
            if existing is not None:
                sub = EmailMessage()
                sub.set_content(existing)
                self._payload.append(sub)
        sub = EmailMessage()
        sub.set_content(content, **kw)
        self._payload.append(sub)
        if "Content-Type" not in self or not self["Content-Type"].startswith("multipart"):
            try:
                del self["Content-Type"]
            except KeyError:
                pass
            self["Content-Type"] = 'multipart/mixed; boundary="===WeavePyBoundary==="'

    def get_body(self, preferencelist=("related", "html", "plain")):
        if not self.is_multipart():
            return self
        for pref in preferencelist:
            for sub in self.walk():
                if sub.get_content_subtype() == pref:
                    return sub
        return None

    def iter_attachments(self):
        if not self.is_multipart():
            return iter(())
        body = self.get_body()
        return iter([sub for sub in self._payload if sub is not body])

    def get_content(self):
        payload = self.get_payload()
        if isinstance(payload, str):
            return payload
        if isinstance(payload, bytes):
            return payload.decode(self.get_charset() or "utf-8")
        return payload
