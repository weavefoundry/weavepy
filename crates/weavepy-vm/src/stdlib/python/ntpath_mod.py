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
    # Accept `os.PathLike` (e.g. `PureWindowsPath`) like CPython, which coerces
    # with `os.fspath` before string-munging.
    s = os.fspath(s)
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


def splitroot(p):
    r"""Split a pathname into drive, root and tail.

    The tail contains anything after the root. Faithful port of CPython
    3.13's pure-Python ``ntpath.splitroot`` (handles drive letters, UNC
    shares, device paths and the ``\\?\UNC\`` prefix)."""
    p = os.fspath(p)
    if isinstance(p, bytes):
        sep = b'\\'
        altsep = b'/'
        colon = b':'
        unc_prefix = b'\\\\?\\UNC\\'
        empty = b''
    else:
        sep = '\\'
        altsep = '/'
        colon = ':'
        unc_prefix = '\\\\?\\UNC\\'
        empty = ''
    normp = p.replace(altsep, sep)
    if normp[:1] == sep:
        if normp[1:2] == sep:
            # UNC drives, e.g. \\server\share or \\?\UNC\server\share
            # Device drives, e.g. \\.\device or \\?\device
            start = 8 if normp[:8].upper() == unc_prefix else 2
            index = normp.find(sep, start)
            if index == -1:
                return p, empty, empty
            index2 = normp.find(sep, index + 1)
            if index2 == -1:
                return p, empty, empty
            return p[:index2], p[index2:index2 + 1], p[index2 + 1:]
        else:
            # Relative path with root, e.g. \Windows
            return empty, p[:1], p[1:]
    elif normp[1:2] == colon:
        if normp[2:3] == sep:
            # Absolute drive-letter path, e.g. X:\Windows
            return p[:2], p[2:3], p[3:]
        else:
            # Relative path with drive, e.g. X:Windows
            return p[:2], empty, p[2:]
    else:
        # Relative path, e.g. Windows
        return empty, empty, p


def splitdrive(p):
    """Split a pathname into drive/UNC sharepoint and relative path."""
    drive, root, tail = splitroot(p)
    return drive, root + tail


_reserved_chars = frozenset(
    {chr(i) for i in range(32)} |
    {'"', '*', ':', '<', '>', '?', '|', '/', '\\'}
)

_reserved_names = frozenset(
    {'CON', 'PRN', 'AUX', 'NUL', 'CONIN$', 'CONOUT$'} |
    {f'COM{c}' for c in '123456789\xb9\xb2\xb3'} |
    {f'LPT{c}' for c in '123456789\xb9\xb2\xb3'}
)


def isreserved(path):
    """Return true if the pathname is reserved by the system."""
    # Refer to "Naming Files, Paths, and Namespaces":
    # https://docs.microsoft.com/en-us/windows/win32/fileio/naming-a-file
    path = os.fsdecode(splitroot(path)[2]).replace(altsep, sep)
    return any(_isreservedname(name) for name in reversed(path.split(sep)))


def _isreservedname(name):
    """Return true if the filename is reserved by the system."""
    # Trailing dots and spaces are reserved.
    if name[-1:] in ('.', ' '):
        return name not in ('.', '..')
    # Wildcards, separators, colon, and pipe (*?"<>/\:|) are reserved.
    # ASCII control characters (0-31) are reserved.
    # Colon is reserved for file streams (e.g. "name:stream[:type]").
    if _reserved_chars.intersection(name):
        return True
    # DOS device names are reserved (e.g. "nul" or "nul .txt").
    return name.partition('.')[0].rstrip(' ').upper() in _reserved_names


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
