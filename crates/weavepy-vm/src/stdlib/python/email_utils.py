"""WeavePy `email.utils` — address / date helpers."""

import time as _time


def formatdate(timeval=None, localtime=False, usegmt=False):
    if timeval is None:
        timeval = _time.time()
    tt = _time.gmtime(timeval) if not localtime else _time.localtime(timeval)
    s = _time.strftime("%a, %d %b %Y %H:%M:%S", tt)
    if usegmt:
        return s + " GMT"
    return s + " -0000"


def format_datetime(dt, usegmt=False):
    return formatdate(time_tuple_from_dt(dt), usegmt=usegmt)


def time_tuple_from_dt(dt):
    return _time.mktime((dt.year, dt.month, dt.day, dt.hour, dt.minute, dt.second, 0, 0, -1))


def parsedate(data):
    if not data:
        return None
    try:
        # CPython is much more flexible. We accept "Day, dd Mon yyyy HH:MM:SS ..."
        bits = data.split()
        if bits[0].endswith(","):
            bits = bits[1:]
        day = int(bits[0])
        month = ["jan", "feb", "mar", "apr", "may", "jun",
                 "jul", "aug", "sep", "oct", "nov", "dec"].index(bits[1][:3].lower()) + 1
        year = int(bits[2])
        time_parts = bits[3].split(":")
        hh = int(time_parts[0])
        mm = int(time_parts[1])
        ss = int(time_parts[2]) if len(time_parts) > 2 else 0
        return (year, month, day, hh, mm, ss, 0, 1, -1)
    except Exception:
        return None


def parsedate_tz(data):
    pd = parsedate(data)
    if pd is None:
        return None
    return pd + (0,)


def mktime_tz(t):
    return _time.mktime(t[:9])


def parseaddr(addr):
    if not addr:
        return ("", "")
    s = str(addr).strip()
    if "<" in s and ">" in s:
        name = s[:s.find("<")].strip().strip('"')
        email = s[s.find("<") + 1:s.rfind(">")]
        return (name, email)
    return ("", s)


def formataddr(pair):
    name, addr = pair
    if name:
        return '"{}" <{}>'.format(name.replace('"', ''), addr)
    return addr


def getaddresses(fieldvalues):
    out = []
    for fv in fieldvalues:
        for part in fv.split(","):
            out.append(parseaddr(part))
    return out


def make_msgid(idstring=None, domain=None):
    import time
    import os
    timeval = int(time.time() * 1000)
    pid = os.getpid() if hasattr(os, "getpid") else 0
    rand = abs(hash((timeval, pid))) & 0xffffffff
    parts = ["{}.{}.{}".format(timeval, pid, rand)]
    if idstring:
        parts.append(idstring)
    domain = domain or "local"
    return "<{}@{}>".format(".".join(parts), domain)


def collapse_rfc2231_value(value, *_):
    return value


def decode_rfc2231(value):
    return ("us-ascii", None, value)


def encode_rfc2231(s, charset=None, language=None):
    if charset is None and language is None:
        return s
    return "{}'{}'{}".format(charset or "", language or "", s)


def unquote(s):
    if s.startswith('"') and s.endswith('"'):
        return s[1:-1]
    return s


def quote(s):
    return '"{}"'.format(s.replace('"', '\\"'))


__all__ = [
    "formatdate", "format_datetime",
    "parsedate", "parsedate_tz", "mktime_tz",
    "parseaddr", "formataddr", "getaddresses",
    "make_msgid", "collapse_rfc2231_value",
    "decode_rfc2231", "encode_rfc2231",
    "unquote", "quote",
]
