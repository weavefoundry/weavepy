"""Minimal but faithful ``locale`` for the portable 'C'/'POSIX' locale.

CPython backs :mod:`locale` with the C ``_locale`` extension. WeavePy ships
a pure-Python module implementing the public surface against the default
'C' locale: querying always succeeds, switching to ``''``/``'C'``/``'POSIX'``
succeeds, and any other (uninstalled) locale raises :class:`Error` exactly
as CPython does on a host where that locale is unavailable (RFC 0037 WS8).
"""

CHAR_MAX = 127

# Category constants. The numeric values are an internal, stable choice
# (CPython's come from the C library and are platform-specific); code should
# use the names, never the literals.
LC_CTYPE = 0
LC_NUMERIC = 1
LC_TIME = 2
LC_COLLATE = 3
LC_MONETARY = 4
LC_MESSAGES = 5
LC_ALL = 6

_ALL_CATEGORIES = (LC_CTYPE, LC_NUMERIC, LC_TIME, LC_COLLATE, LC_MONETARY,
                   LC_MESSAGES)

__all__ = [
    "getlocale", "getdefaultlocale", "getpreferredencoding", "Error",
    "setlocale", "resetlocale", "localeconv", "strcoll", "strxfrm",
    "str", "atof", "atoi", "format_string", "currency", "normalize",
    "LC_CTYPE", "LC_NUMERIC", "LC_TIME", "LC_COLLATE", "LC_MONETARY",
    "LC_MESSAGES", "LC_ALL", "CHAR_MAX", "delocalize", "localize",
]


class Error(Exception):
    pass


_state = {c: "C" for c in (LC_CTYPE, LC_NUMERIC, LC_TIME, LC_COLLATE,
                           LC_MONETARY, LC_MESSAGES, LC_ALL)}


def _norm_requested(value):
    """Map a requested locale name to the only locale we can honour ('C')
    or raise :class:`Error` for anything we cannot install — the same
    observable contract as CPython on a host missing that locale."""
    if isinstance(value, tuple):
        value = _build_localename(value)
    if value in ("", "C", "POSIX"):
        return "C"
    raise Error("unsupported locale setting")


def setlocale(category, locale=None):
    if category not in (LC_ALL, *_ALL_CATEGORIES):
        raise Error("invalid locale category")
    if locale is None:
        return _state.get(category, "C")
    normalized = _norm_requested(locale)
    if category == LC_ALL:
        for c in (LC_ALL, *_ALL_CATEGORIES):
            _state[c] = normalized
    else:
        _state[category] = normalized
    return normalized


def resetlocale(category=LC_ALL):
    setlocale(category, "C")


def localeconv():
    """Return the lconv table for the 'C' locale."""
    return {
        "decimal_point": ".",
        "thousands_sep": "",
        "grouping": [],
        "int_curr_symbol": "",
        "currency_symbol": "",
        "mon_decimal_point": "",
        "mon_thousands_sep": "",
        "mon_grouping": [],
        "positive_sign": "",
        "negative_sign": "",
        "int_frac_digits": CHAR_MAX,
        "frac_digits": CHAR_MAX,
        "p_cs_precedes": CHAR_MAX,
        "p_sep_by_space": CHAR_MAX,
        "n_cs_precedes": CHAR_MAX,
        "n_sep_by_space": CHAR_MAX,
        "p_sign_posn": CHAR_MAX,
        "n_sign_posn": CHAR_MAX,
    }


def getlocale(category=LC_CTYPE):
    """The 'C' locale carries no language/encoding pair."""
    return (None, None)


def getdefaultlocale(envvars=("LC_ALL", "LC_CTYPE", "LANG", "LANGUAGE")):
    return (None, None)


def getpreferredencoding(do_setlocale=True):
    return "utf-8"


def getencoding():
    return "utf-8"


def normalize(localename):
    # Without the C alias table we only recognise the portable names.
    name = localename.lower()
    if name in ("c", "posix", ""):
        return "C"
    return localename


def _build_localename(localetuple):
    try:
        language, encoding = localetuple
    except (TypeError, ValueError):
        raise TypeError("Locale must be None, a string, or an iterable of "
                        "two strings -- language code, encoding.") from None
    if language is None:
        language = "C"
    if encoding is None:
        return language
    return language + "." + encoding


def strcoll(a, b):
    return (a > b) - (a < b)


def strxfrm(s):
    return s


# --- numeric helpers (C locale: '.' decimal point, no grouping) -----------

def localize(string, grouping=False, monetary=False):
    return string


def delocalize(string):
    return string


def atof(string, func=float):
    return func(delocalize(string))


def atoi(string):
    return int(delocalize(string))


def str(val):
    return format_string("%.12g", val)


def format_string(format, val, grouping=False, monetary=False):
    import re as _re

    def _strip(m):
        return m.group(0)

    # In the 'C' locale there is no grouping or monetary decoration, so the
    # conversion is just plain printf-style formatting.
    if isinstance(val, tuple):
        return format % val
    return format % val


def format(percent, value, grouping=False, monetary=False, *additional):
    if additional:
        formatted = percent % ((value,) + additional)
    else:
        formatted = percent % value
    return formatted


def currency(val, symbol=True, grouping=False, international=False):
    raise ValueError("Currency formatting is not possible in the 'C' locale.")
