"""WeavePy `html` — minimal escape / unescape helpers.

Just the public top-level surface: `escape`, `unescape`. The
sub-packages `html.parser`, `html.entities` are intentionally not
shipped yet — the most-imported helpers here are the escape
functions.
"""


def escape(s, quote=True):
    s = s.replace("&", "&amp;")
    s = s.replace("<", "&lt;")
    s = s.replace(">", "&gt;")
    if quote:
        s = s.replace('"', "&quot;")
        s = s.replace("'", "&#x27;")
    return s


_NAMED = {
    "amp": "&",
    "lt": "<",
    "gt": ">",
    "quot": '"',
    "apos": "'",
    "nbsp": "\u00a0",
    "copy": "\u00a9",
    "reg": "\u00ae",
    "deg": "\u00b0",
    "trade": "\u2122",
}


def unescape(s):
    """Reverse `escape()` plus the common HTML entities."""
    # We walk the string manually since WeavePy's `re.sub` doesn't
    # accept a callable `repl` yet. The state machine here is small
    # enough that a hand-rolled loop is clear.
    out = []
    i = 0
    n = len(s)
    while i < n:
        c = s[i]
        if c != "&":
            out.append(c)
            i += 1
            continue
        # Find the closing semicolon — bail out if absent.
        semi = s.find(";", i + 1)
        if semi == -1 or semi - i > 16:
            out.append(c)
            i += 1
            continue
        body = s[i + 1:semi]
        replaced = None
        if body.startswith("#"):
            try:
                if body[1:2].lower() == "x":
                    replaced = chr(int(body[2:], 16))
                else:
                    replaced = chr(int(body[1:]))
            except ValueError:
                replaced = None
        else:
            replaced = _NAMED.get(body)
        if replaced is None:
            out.append(c)
            i += 1
        else:
            out.append(replaced)
            i = semi + 1
    return "".join(out)


__all__ = ["escape", "unescape"]
