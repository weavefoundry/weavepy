"""Common path operations shared by both POSIX and NT path modules.

WeavePy port of CPython's ``genericpath``.
"""

import os
import stat


def exists(path):
    try:
        os.stat(path)
    except (OSError, ValueError):
        return False
    return True


def isfile(path):
    try:
        st = os.stat(path)
    except (OSError, ValueError):
        return False
    return stat.S_ISREG(st.st_mode)


def isdir(s):
    try:
        st = os.stat(s)
    except (OSError, ValueError):
        return False
    return stat.S_ISDIR(st.st_mode)


def islink(path):
    try:
        st = os.lstat(path)
    except (OSError, AttributeError, ValueError):
        return False
    return stat.S_ISLNK(st.st_mode)


def getsize(filename):
    return os.stat(filename).st_size


def getmtime(filename):
    return os.stat(filename).st_mtime


def getatime(filename):
    return os.stat(filename).st_atime


def getctime(filename):
    return os.stat(filename).st_ctime


def commonprefix(m):
    if not m:
        return ""
    try:
        s1 = min(m)
        s2 = max(m)
    except (TypeError, ValueError):
        s1 = m[0]
        s2 = m[0]
        for x in m:
            if x < s1:
                s1 = x
            if x > s2:
                s2 = x
    for i, c in enumerate(s1):
        if c != s2[i]:
            return s1[:i]
    return s1


def samestat(s1, s2):
    return s1.st_ino == s2.st_ino and s1.st_dev == s2.st_dev


def samefile(f1, f2):
    s1 = os.stat(f1)
    s2 = os.stat(f2)
    return samestat(s1, s2)


def _splitext(p, sep, altsep, extsep):
    """Split ``p`` into (root, ext) on the last ``extsep``."""
    sep_index = p.rfind(sep)
    if altsep:
        sep_index = max(sep_index, p.rfind(altsep))
    dot_index = p.rfind(extsep)
    if dot_index > sep_index:
        # skip all leading dots
        filename_index = sep_index + 1
        while filename_index < dot_index:
            if p[filename_index:filename_index + 1] != extsep:
                return p[:dot_index], p[dot_index:]
            filename_index += 1
    return p, p[:0]


def _check_arg_types(funcname, *args):
    hasstr = hasbytes = False
    for s in args:
        if isinstance(s, str):
            hasstr = True
        elif isinstance(s, (bytes, bytearray)):
            hasbytes = True
        else:
            raise TypeError(
                "{}() argument must be str, bytes, or os.PathLike object, not {!r}"
                .format(funcname, type(s).__name__))
    if hasstr and hasbytes:
        raise TypeError("Can't mix strings and bytes in path components")
