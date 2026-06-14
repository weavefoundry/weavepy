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


def rmtree(path, ignore_errors=False, onerror=None, *, onexc=None, dir_fd=None):
    """Recursively delete a directory tree.

    Mirrors the CPython 3.12+ signature: ``onexc`` (preferred) and the
    legacy ``onerror`` are error callbacks, used in that priority order.
    ``tempfile.TemporaryDirectory`` cleanup drives us via ``onexc``.
    """
    if dir_fd is not None:
        raise NotImplementedError("rmtree: dir_fd is not supported")
    try:
        _shutil.rmtree(os.fspath(path))
    except OSError as exc:
        if ignore_errors:
            return
        if onexc is not None:
            onexc(rmtree, path, exc)
        elif onerror is not None:
            import sys
            onerror(rmtree, path, sys.exc_info())
        else:
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


from collections import namedtuple as _namedtuple

# CPython returns ``os.terminal_size``, a struct-sequence exposing both
# index access (``size[0]``) and the ``.columns`` / ``.lines`` attributes.
# A namedtuple gives the same surface, which callers like ``argparse``
# (``shutil.get_terminal_size().columns``) depend on.
terminal_size = _namedtuple("terminal_size", ["columns", "lines"])


def get_terminal_size(fallback=(80, 24)):
    """Return terminal size as a ``terminal_size(columns, lines)``.

    Faithful to CPython: the ``COLUMNS`` / ``LINES`` environment variables
    win when set to a positive value, and a non-positive (or unset/invalid)
    value falls through to the OS query. Reading ``os.environ`` here — rather
    than the process env that the Rust ``_shutil`` core sees — is what lets
    the stdlib tests' ``os.environ['COLUMNS'] = '80'`` overrides take effect,
    and it neutralises a leaked ``COLUMNS=0`` (which would otherwise wrap
    every usage line to a single column).
    """
    try:
        columns = int(os.environ['COLUMNS'])
    except (KeyError, ValueError):
        columns = 0
    try:
        lines = int(os.environ['LINES'])
    except (KeyError, ValueError):
        lines = 0

    if columns <= 0 or lines <= 0:
        try:
            c, l = _shutil.get_terminal_size()
        except Exception:
            c, l = fallback
        if columns <= 0:
            columns = c
        if lines <= 0:
            lines = l

    # ``_make`` consumes an iterable, matching ``os.terminal_size``'s
    # struct-sequence constructor (a plain ``terminal_size((c, l))`` would
    # be read as a single positional arg and fail).
    return terminal_size._make((columns, lines))


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
