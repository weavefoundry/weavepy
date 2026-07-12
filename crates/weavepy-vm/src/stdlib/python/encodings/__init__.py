"""WeavePy frozen subset of the CPython encodings package.

Only the modules WeavePy resolves through codecs.lookup live here
(idna, punycode, aliases, and on-demand charmap codepages). The bulk of
the encodings are served natively, so this package is intentionally not
the codec search bootstrap. `normalize_encoding` and the `aliases`
registry match CPython for consumers that use them directly
(`locale`, `email.charset`, third-party sniffers).
"""

from . import aliases

# CPython's search-function cache ({normalised name: CodecInfo}). WeavePy's
# real cache lives in the frozen `codecs` module, but tests (and refleak
# hygiene helpers) pop from `encodings._cache` directly.
_cache = {}
_unknown = '--unknown--'
_MAXCACHE = 500


def normalize_encoding(encoding):
    """Normalize an encoding name (CPython semantics).

    Collapse runs of punctuation to a single underscore, keep
    alphanumerics and ``.``, drop non-ASCII characters. Leaves case
    unchanged: ``'UTF-16LE'`` → ``'UTF_16LE'``, ``'latin 1'`` →
    ``'latin_1'``.
    """
    if isinstance(encoding, bytes):
        encoding = str(encoding, "ascii")
    chars = []
    punct = False
    for c in encoding:
        if c.isalnum() or c == ".":
            if punct and chars:
                chars.append("_")
            if c.isascii():
                chars.append(c)
            punct = False
        else:
            punct = True
    return "".join(chars)
