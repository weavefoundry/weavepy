"""Common POSIX-style path manipulations — WeavePy port of CPython's
``posixpath``.

The module is also imported as ``os.path`` on POSIX platforms.
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
sep = "/"
pathsep = ":"
defpath = "/bin:/usr/bin"
altsep = None
devnull = "/dev/null"


def _get_sep(path):
    return b"/" if isinstance(path, bytes) else "/"


def normcase(s):
    if not isinstance(s, (bytes, str)):
        raise TypeError("normcase: expected str, bytes or os.PathLike")
    return s


def isabs(s):
    sep = _get_sep(s)
    return s.startswith(sep)


def join(a, *p):
    sep = _get_sep(a)
    path = a
    for b in p:
        if isinstance(b, bytes) and not isinstance(path, bytes):
            raise TypeError("can't mix bytes and non-bytes in path components")
        if b.startswith(sep):
            path = b
        elif not path or path.endswith(sep):
            path += b
        else:
            path += sep + b
    return path


def split(p):
    sep = _get_sep(p)
    i = p.rfind(sep) + 1
    head, tail = p[:i], p[i:]
    if head and head != sep * len(head):
        head = head.rstrip(sep)
    return head, tail


def splitext(p):
    sep_ = _get_sep(p)
    if isinstance(p, bytes):
        return _splitext(p, sep_, None, b".")
    return _splitext(p, sep_, None, ".")


def splitdrive(p):
    return p[:0], p


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


def ismount(path):
    try:
        s1 = os.lstat(path)
    except OSError:
        return False
    if stat.S_ISLNK(s1.st_mode):
        return False
    parent = join(path, b".." if isinstance(path, bytes) else "..")
    try:
        s2 = os.lstat(parent)
    except OSError:
        return False
    if s1.st_dev != s2.st_dev:
        return True
    if s1.st_ino == s2.st_ino:
        return True
    return False


def expanduser(path):
    if isinstance(path, bytes):
        tilde = b"~"
        sep = b"/"
    else:
        tilde = "~"
        sep = "/"
    if not path.startswith(tilde):
        return path
    i = path.find(sep, 1)
    if i < 0:
        i = len(path)
    if i == 1:
        home = os.environ.get("HOME")
        if not home:
            return path
    else:
        return path
    if isinstance(path, bytes):
        home = home.encode("utf-8") if isinstance(home, str) else home
    return home + path[i:]


def expandvars(path):
    import re
    if isinstance(path, bytes):
        if b"$" not in path and b"{" not in path:
            return path
        pattern = rb"\$(\w+|\{[^}]*\})"
    else:
        if "$" not in path and "{" not in path:
            return path
        pattern = r"\$(\w+|\{[^}]*\})"
    out = []
    pos = 0
    for m in re.finditer(pattern, path):
        out.append(path[pos:m.start()])
        name = m.group(1)
        if isinstance(name, bytes):
            name_s = name.decode("utf-8")
        else:
            name_s = name
        if name_s.startswith("{") and name_s.endswith("}"):
            name_s = name_s[1:-1]
        val = os.environ.get(name_s)
        if val is None:
            out.append(m.group(0))
        else:
            if isinstance(path, bytes):
                out.append(val.encode("utf-8"))
            else:
                out.append(val)
        pos = m.end()
    out.append(path[pos:])
    if isinstance(path, bytes):
        return b"".join(out)
    return "".join(out)


def normpath(path):
    sep_ = _get_sep(path)
    if isinstance(path, bytes):
        empty = b""
        dot = b"."
        dotdot = b".."
    else:
        empty = ""
        dot = "."
        dotdot = ".."
    if path == empty:
        return dot
    initial_slashes = 1 if path.startswith(sep_) else 0
    if (initial_slashes
            and path.startswith(sep_ * 2)
            and not path.startswith(sep_ * 3)):
        initial_slashes = 2
    comps = path.split(sep_)
    new_comps = []
    for comp in comps:
        if comp in (empty, dot):
            continue
        if (comp != dotdot or (not initial_slashes and not new_comps)
                or (new_comps and new_comps[-1] == dotdot)):
            new_comps.append(comp)
        elif new_comps:
            new_comps.pop()
    comps = new_comps
    path = sep_.join(comps)
    if initial_slashes:
        path = sep_ * initial_slashes + path
    return path or dot


def abspath(path):
    if not isabs(path):
        try:
            cwd = os.getcwd()
        except OSError:
            cwd = "/"
        if isinstance(path, bytes):
            cwd = cwd.encode("utf-8") if isinstance(cwd, str) else cwd
        path = join(cwd, path)
    return normpath(path)


def realpath(path, *, strict=False):
    # Simple resolver: walk symlinks until stable or strict failure.
    seen = {}
    path = abspath(path)
    parts = path.split(sep)
    resolved = sep if path.startswith(sep) else ""
    for part in parts:
        if part in ("", "."):
            continue
        if part == "..":
            resolved = dirname(resolved.rstrip(sep)) or sep
            continue
        candidate = join(resolved, part)
        if candidate in seen:
            return resolved
        seen[candidate] = True
        try:
            target = os.readlink(candidate)
        except (OSError, NotImplementedError, AttributeError):
            resolved = candidate
            continue
        if isabs(target):
            resolved = target
        else:
            resolved = join(resolved, target)
    return resolved or sep


def relpath(path, start=None):
    if start is None:
        start = curdir
    if not path:
        raise ValueError("no path specified")
    try:
        start_abs = abspath(start)
        path_abs = abspath(path)
        start_list = [x for x in start_abs.split(sep) if x]
        path_list = [x for x in path_abs.split(sep) if x]
        i = 0
        while i < len(start_list) and i < len(path_list) and start_list[i] == path_list[i]:
            i += 1
        rel = [pardir] * (len(start_list) - i) + path_list[i:]
        if not rel:
            return curdir
        return sep.join(rel)
    except (TypeError, AttributeError, BytesWarning, DeprecationWarning):
        raise


def commonpath(paths):
    if not paths:
        raise ValueError("commonpath() arg is an empty sequence")
    split_paths = [p.split(sep) for p in paths]
    try:
        isabs_, = set(p[:1] == [""] for p in split_paths)
    except ValueError as e:
        raise ValueError("Can't mix absolute and relative paths") from e
    split_paths = [[c for c in s if c and c != "."] for s in split_paths]
    s1 = min(split_paths)
    s2 = max(split_paths)
    common = s1
    for i, c in enumerate(s1):
        if c != s2[i]:
            common = s1[:i]
            break
    prefix = sep if isabs_ else sep[:0]
    return prefix + sep.join(common)


supports_unicode_filenames = False

__all__ = [
    "curdir", "pardir", "extsep", "sep", "pathsep", "defpath",
    "altsep", "devnull",
    "normcase", "isabs", "join", "split", "splitext", "splitdrive",
    "basename", "dirname", "lexists", "ismount",
    "expanduser", "expandvars", "normpath", "abspath", "realpath",
    "relpath", "commonpath",
    "exists", "getatime", "getctime", "getmtime", "getsize",
    "isdir", "isfile", "islink",
    "samefile", "samestat", "commonprefix",
    "supports_unicode_filenames",
]
