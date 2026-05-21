"""WeavePy `urllib.error` — exception types."""


class URLError(OSError):
    """Raised when a URL cannot be opened."""

    def __init__(self, reason, filename=None):
        super().__init__(reason)
        self.reason = reason
        self.filename = filename
        self.args = (reason,)

    def __str__(self):
        return "<urlopen error {}>".format(self.reason)


class HTTPError(URLError):
    """Raised when an HTTP request returns an error status."""

    def __init__(self, url, code, msg, hdrs, fp):
        self.code = code
        self.msg = msg
        self.hdrs = hdrs
        self.fp = fp
        self.filename = url
        URLError.__init__(self, msg, url)
        self.url = url
        self.args = (code, msg, url)

    def __str__(self):
        return "HTTP Error {}: {}".format(self.code, self.msg)


class ContentTooShortError(URLError):
    """Raised when fewer bytes than expected were received."""

    def __init__(self, message, content):
        URLError.__init__(self, message)
        self.content = content


__all__ = ["URLError", "HTTPError", "ContentTooShortError"]
