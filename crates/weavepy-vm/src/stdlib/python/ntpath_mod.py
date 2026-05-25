"""Common Windows path manipulations — WeavePy port of CPython's
``ntpath``.

Imported as ``os.path`` on Windows. On other platforms it's
available but rarely used; we ship it for portability.
"""

import os
import stat
import genericpath
from genericpath import (
    commonprefix,
    exists,
    getatime,
    getctime,
    getmtime,
    getsize,
    isdir,
    isfile,
    islink,
    samefile,
    samestat,
    _splitext,
)

curdir = "."
pardir = ".."
extsep = "."
sep = "\\"
altsep = "/"
pathsep = ";"
defpath = ".;C:\\bin"
devnull = "nul"


def _get_bothseps(path):
    if isinstance(path, bytes):
        return b"\\/"
    return "\\/"


def normcase(s):
    if isinstance(s, bytes):
        return s.replace(b"/", b"\\").lower()
    return s.replace("/", "\\").lower()


def isabs(s):
    if isinstance(s, bytes):
        s = s.replace(b"/", b"\\")
        if s.startswith(b"\\\\"):
            return True
        return len(s) >= 3 and s[1:2] == b":" and s[2:3] == b"\\"
    s = s.replace("/", "\\")
    if s.startswith("\\\\"):
        return True
    return len(s) >= 3 and s[1:2] == ":" and s[2:3] == "\\"


def join(path, *paths):
    if isinstance(path, bytes):
        sep_ = b"\\"
        seps = b"\\/"
        colon = b":"
    else:
        sep_ = "\\"
        seps = "\\/"
        colon = ":"
    if not paths:
        return path
    result_drive, result_path = splitdrive(path)
    for p in paths:
        p_drive, p_path = splitdrive(p)
        if p_path and p_path[:1] in seps:
            if p_drive or not result_drive:
                result_drive = p_drive
            result_path = p_path
            continue
        elif p_drive and p_drive != result_drive:
            if p_drive.lower() != result_drive.lower():
                result_drive = p_drive
                result_path = p_path
                continue
            result_drive = p_drive
        if result_path and result_path[-1:] not in seps:
            result_path = result_path + sep_
        result_path = result_path + p_path
    if (result_path and result_path[:1] not in seps
            and result_drive and result_drive[-1:] != colon):
        return result_drive + sep_ + result_path
    return result_drive + result_path


def splitdrive(p):
    if isinstance(p, bytes):
        empty = b""
        colon = b":"
    else:
        empty = ""
        colon = ":"
    if len(p) >= 2 and p[1:2] == colon:
        return p[:2], p[2:]
    return empty, p


def split(p):
    seps = _get_bothseps(p)
    d, p = splitdrive(p)
    i = len(p)
    while i and p[i - 1] not in seps:
        i -= 1
    head, tail = p[:i], p[i:]
    head = head.rstrip(seps) if head and head != seps[:1] * len(head) else head
    return d + head, tail


def splitext(p):
    if isinstance(p, bytes):
        return _splitext(p, b"\\", b"/", b".")
    return _splitext(p, "\\", "/", ".")


def basename(p):
    return split(p)[1]


def dirname(p):
    return split(p)[0]


def lexists(path):
    try:
        os.lstat(path)
    except (OSError, ValueError):
        return False
    return True


def expanduser(path):
    if isinstance(path, bytes):
        tilde = b"~"
        seps = b"\\/"
    else:
        tilde = "~"
        seps = "\\/"
    if not path.startswith(tilde):
        return path
    i, n = 1, len(path)
    while i < n and path[i:i + 1] not in seps:
        i += 1
    if i == 1:
        userhome = os.environ.get("USERPROFILE") or os.environ.get("HOME") or ""
    else:
        return path
    if isinstance(path, bytes):
        userhome = userhome.encode("utf-8") if isinstance(userhome, str) else userhome
    return userhome + path[i:]


def expandvars(path):
    return path


def normpath(path):
    if isinstance(path, bytes):
        sep_ = b"\\"
        altsep_ = b"/"
        curdir_ = b"."
        pardir_ = b".."
        special = (b"\\\\.\\", b"\\\\?\\")
    else:
        sep_ = "\\"
        altsep_ = "/"
        curdir_ = "."
        pardir_ = ".."
        special = ("\\\\.\\", "\\\\?\\")
    if path.startswith(special):
        return path
    path = path.replace(altsep_, sep_)
    drive, path = splitdrive(path)
    if path.startswith(sep_):
        drive = drive + sep_
        path = path.lstrip(sep_)
    comps = path.split(sep_)
    new_comps = []
    for comp in comps:
        if comp in (curdir_, b"" if isinstance(path, bytes) else ""):
            continue
        if comp == pardir_:
            if new_comps and new_comps[-1] != pardir_:
                new_comps.pop()
                continue
            if drive.endswith(sep_):
                continue
        new_comps.append(comp)
    return drive + sep_.join(new_comps) or curdir_


def abspath(path):
    if not isabs(path):
        try:
            cwd = os.getcwd()
        except OSError:
            cwd = "."
        if isinstance(path, bytes):
            cwd = cwd.encode("utf-8") if isinstance(cwd, str) else cwd
        path = join(cwd, path)
    return normpath(path)


def realpath(path, *, strict=False):
    return abspath(path)


def relpath(path, start=None):
    if start is None:
        start = curdir
    return path


def commonpath(paths):
    if not paths:
        raise ValueError("commonpath() arg is an empty sequence")
    return commonprefix(paths)


def ismount(path):
    seps = _get_bothseps(path)
    path = abspath(path)
    return path == path.rstrip(seps) + (b"\\" if isinstance(path, bytes) else "\\")


supports_unicode_filenames = True

__all__ = [
    "curdir", "pardir", "extsep", "sep", "pathsep", "defpath", "altsep",
    "devnull", "normcase", "isabs", "join", "splitdrive", "split",
    "splitext", "basename", "dirname", "lexists",
    "expanduser", "expandvars", "normpath", "abspath", "realpath",
    "relpath", "commonpath", "commonprefix", "ismount",
    "exists", "getatime", "getctime", "getmtime", "getsize",
    "isdir", "isfile", "islink",
    "samefile", "samestat",
    "supports_unicode_filenames",
]
