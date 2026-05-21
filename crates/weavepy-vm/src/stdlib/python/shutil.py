"""WeavePy `shutil` — convenience layer over `_shutil`.

The Rust `_shutil` core provides the bulk-filesystem primitives;
this module dresses them up with the CPython surface: `copy`,
`copy2`, `copyfile`, `copytree`, `move`, `rmtree`, `which`,
`disk_usage`, `get_terminal_size`, and the `Error` exception.
"""

import _shutil
import os


class Error(OSError):
    """Raised by `shutil` operations on consolidated failure."""


def copyfile(src, dst, *, follow_symlinks=True):
    """Copy data from `src` to `dst`. Returns the destination path."""
    return _shutil.copyfile(src, dst)


def copy(src, dst, *, follow_symlinks=True):
    """Copy data and mode bits from `src` to `dst`."""
    if os.path.isdir(dst):
        dst = os.path.join(dst, os.path.basename(src))
    return _shutil.copyfile(src, dst)


def copy2(src, dst, *, follow_symlinks=True):
    """Like `copy`, but also preserves metadata. We approximate
    metadata preservation — full xattr / ACL copying is out of
    scope."""
    return copy(src, dst, follow_symlinks=follow_symlinks)


def copytree(src, dst, *, symlinks=False, ignore=None, copy_function=None,
             ignore_dangling_symlinks=False, dirs_exist_ok=False):
    """Recursively copy a directory tree."""
    return _shutil.copytree(src, dst)


def rmtree(path, ignore_errors=False, onerror=None):
    """Recursively delete a directory tree."""
    try:
        _shutil.rmtree(path)
    except OSError:
        if not ignore_errors:
            raise


def move(src, dst):
    """Move a file or directory."""
    if os.path.isdir(dst):
        dst = os.path.join(dst, os.path.basename(src))
    try:
        os.rename(src, dst)
    except OSError:
        # Fall back to copy + remove for cross-filesystem moves.
        if os.path.isdir(src):
            copytree(src, dst)
            rmtree(src)
        else:
            copyfile(src, dst)
            os.remove(src)
    return dst


def which(cmd, mode=None, path=None):
    """Locate `cmd` on PATH."""
    return _shutil.which(cmd, path)


def disk_usage(path):
    """`(total, used, free)` disk usage information."""
    total, used, free = _shutil.disk_usage(path)

    class _Usage:
        pass
    u = _Usage()
    u.total = total
    u.used = used
    u.free = free
    return u


def get_terminal_size(fallback=(80, 24)):
    """Return terminal size as `(columns, lines)`."""
    try:
        cols, lines = _shutil.get_terminal_size()
        return (cols, lines)
    except Exception:
        return fallback


def copyfileobj(fsrc, fdst, length=16 * 1024):
    """Copy data from `fsrc` to `fdst` in `length`-byte chunks."""
    while True:
        chunk = fsrc.read(length)
        if not chunk:
            break
        fdst.write(chunk)


__all__ = [
    "Error", "copy", "copy2", "copyfile", "copyfileobj", "copytree",
    "rmtree", "move", "which", "disk_usage", "get_terminal_size",
]
