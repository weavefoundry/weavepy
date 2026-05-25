"""Constants and functions for inspecting ``os.stat`` results.

WeavePy port of CPython's ``stat`` module. Mirrors the public
constants and the ``S_IS*`` predicate helpers.
"""

# File-type bits.
S_IFMT = 0o170000
S_IFDIR = 0o040000
S_IFCHR = 0o020000
S_IFBLK = 0o060000
S_IFREG = 0o100000
S_IFIFO = 0o010000
S_IFLNK = 0o120000
S_IFSOCK = 0o140000
S_IFDOOR = 0o150000
S_IFPORT = 0o160000
S_IFWHT = 0o160000

# Permission bits.
S_ISUID = 0o4000
S_ISGID = 0o2000
S_ISVTX = 0o1000
S_IRWXU = 0o700
S_IRUSR = 0o400
S_IWUSR = 0o200
S_IXUSR = 0o100
S_IRWXG = 0o070
S_IRGRP = 0o040
S_IWGRP = 0o020
S_IXGRP = 0o010
S_IRWXO = 0o007
S_IROTH = 0o004
S_IWOTH = 0o002
S_IXOTH = 0o001

# Common aliases.
S_IREAD = S_IRUSR
S_IWRITE = S_IWUSR
S_IEXEC = S_IXUSR

ST_MODE = 0
ST_INO = 1
ST_DEV = 2
ST_NLINK = 3
ST_UID = 4
ST_GID = 5
ST_SIZE = 6
ST_ATIME = 7
ST_MTIME = 8
ST_CTIME = 9


def S_IMODE(mode):
    return mode & 0o7777


def S_IFMT(mode):  # noqa: F811 — overrides constant intentionally per CPython.
    return mode & 0o170000


def S_ISDIR(mode):
    return S_IFMT(mode) == S_IFDIR


def S_ISCHR(mode):
    return S_IFMT(mode) == S_IFCHR


def S_ISBLK(mode):
    return S_IFMT(mode) == S_IFBLK


def S_ISREG(mode):
    return S_IFMT(mode) == S_IFREG


def S_ISFIFO(mode):
    return S_IFMT(mode) == S_IFIFO


def S_ISLNK(mode):
    return S_IFMT(mode) == S_IFLNK


def S_ISSOCK(mode):
    return S_IFMT(mode) == S_IFSOCK


def S_ISDOOR(mode):
    return False


def S_ISPORT(mode):
    return False


def S_ISWHT(mode):
    return False


# Flags returned by `stat.filemode()`.
_FILETYPE_CHARS = (
    (S_IFLNK, "l"),
    (S_IFREG, "-"),
    (S_IFBLK, "b"),
    (S_IFDIR, "d"),
    (S_IFCHR, "c"),
    (S_IFIFO, "p"),
)


def filemode(mode):
    perm = []
    for fmt_bits, ch in _FILETYPE_CHARS:
        if S_IFMT(mode) == fmt_bits:
            perm.append(ch)
            break
    else:
        perm.append("?")
    for who, bits in (("USR", (S_IRUSR, S_IWUSR, S_IXUSR, S_ISUID)),
                      ("GRP", (S_IRGRP, S_IWGRP, S_IXGRP, S_ISGID)),
                      ("OTH", (S_IROTH, S_IWOTH, S_IXOTH, S_ISVTX))):
        r, w, x, special = bits
        perm.append("r" if mode & r else "-")
        perm.append("w" if mode & w else "-")
        if mode & special:
            if mode & x:
                perm.append("s" if who != "OTH" else "t")
            else:
                perm.append("S" if who != "OTH" else "T")
        else:
            perm.append("x" if mode & x else "-")
    return "".join(perm)


# File-attribute flag bits used by Windows. Provided for completeness
# even on POSIX so cross-platform code keeps importing cleanly.
FILE_ATTRIBUTE_ARCHIVE = 32
FILE_ATTRIBUTE_COMPRESSED = 2048
FILE_ATTRIBUTE_DEVICE = 64
FILE_ATTRIBUTE_DIRECTORY = 16
FILE_ATTRIBUTE_ENCRYPTED = 16384
FILE_ATTRIBUTE_HIDDEN = 2
FILE_ATTRIBUTE_INTEGRITY_STREAM = 32768
FILE_ATTRIBUTE_NORMAL = 128
FILE_ATTRIBUTE_NOT_CONTENT_INDEXED = 8192
FILE_ATTRIBUTE_NO_SCRUB_DATA = 131072
FILE_ATTRIBUTE_OFFLINE = 4096
FILE_ATTRIBUTE_READONLY = 1
FILE_ATTRIBUTE_REPARSE_POINT = 1024
FILE_ATTRIBUTE_SPARSE_FILE = 512
FILE_ATTRIBUTE_SYSTEM = 4
FILE_ATTRIBUTE_TEMPORARY = 256
FILE_ATTRIBUTE_VIRTUAL = 65536


__all__ = [
    name for name in globals()
    if isinstance(name, str)
    and (name.startswith("S_") or name.startswith("ST_") or name.startswith("FILE_"))
]
__all__ += ["filemode"]
