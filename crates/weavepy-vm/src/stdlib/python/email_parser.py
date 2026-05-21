"""WeavePy `email.parser` — RFC 5322-ish parser."""

from email.message import Message


def _parse(text):
    msg = Message()
    if "\r\n" in text and "\n\r\n" not in text:
        sep = "\r\n\r\n"
        line_sep = "\r\n"
    else:
        sep = "\n\n"
        line_sep = "\n"
    head_end = text.find(sep)
    if head_end == -1:
        head = text
        body = ""
    else:
        head = text[:head_end]
        body = text[head_end + len(sep):]
    last_key = None
    last_val = []
    for line in head.split(line_sep):
        if not line:
            continue
        if line[0] in (" ", "\t") and last_key is not None:
            last_val.append(line.lstrip())
            continue
        if last_key is not None:
            msg[last_key] = " ".join(last_val)
        if ":" not in line:
            continue
        k, _, v = line.partition(":")
        last_key = k.strip()
        last_val = [v.strip()]
    if last_key is not None:
        msg[last_key] = " ".join(last_val)
    msg.set_payload(body)
    return msg


class Parser:
    def __init__(self, _class=Message, *, policy=None):
        self._class = _class

    def parsestr(self, text, headersonly=False):
        msg = _parse(text)
        if headersonly:
            msg.set_payload(None)
        return msg

    def parse(self, fp, headersonly=False):
        return self.parsestr(fp.read(), headersonly)


class BytesParser:
    def __init__(self, _class=Message, *, policy=None):
        self._parser = Parser(_class, policy=policy)

    def parsebytes(self, b, headersonly=False):
        if isinstance(b, (bytes, bytearray)):
            text = b.decode("iso-8859-1", errors="replace")
        else:
            text = b
        return self._parser.parsestr(text, headersonly)

    def parse(self, fp, headersonly=False):
        return self.parsebytes(fp.read(), headersonly)


class FeedParser:
    def __init__(self, _factory=Message, *, policy=None):
        self._buf = []
        self._factory = _factory

    def feed(self, data):
        self._buf.append(data)

    def close(self):
        return _parse("".join(self._buf))


__all__ = ["Parser", "BytesParser", "FeedParser"]
