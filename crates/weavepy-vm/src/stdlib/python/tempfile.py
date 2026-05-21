"""WeavePy `tempfile` — convenience layer over the Rust `_tempfile` core.

Provides:
* `gettempdir()`, `gettempprefix()` — from the Rust core.
* `mkstemp(suffix=None, prefix='tmp', dir=None, text=False)` —
  returns `(fd_or_path, path)` matching CPython for the path
  argument; the integer fd return is replaced with a string path
  because we don't expose raw fds today.
* `mkdtemp(suffix=None, prefix='tmp', dir=None)` — atomically
  creates a fresh temp directory.
* `NamedTemporaryFile(mode='w+b', delete=True, suffix=None, prefix='tmp', dir=None)` —
  context-manager-friendly handle.
* `TemporaryDirectory(suffix=None, prefix='tmp', dir=None)` —
  context-manager-friendly handle that `rmtree`s on `__exit__`.
"""

import _tempfile
import os


gettempdir = _tempfile.gettempdir
gettempprefix = _tempfile.gettempprefix


def mkstemp(suffix=None, prefix=None, dir=None, text=False):
    """Create a unique temporary file. Returns `(path, path)`."""
    return _tempfile.mkstemp(suffix, prefix, dir, text)


def mkdtemp(suffix=None, prefix=None, dir=None):
    """Create a unique temporary directory and return its path."""
    return _tempfile.mkdtemp(suffix, prefix, dir)


class _NamedTempFile:
    """Wraps an `open()`ed file with a `name` attribute and (optional)
    delete-on-close semantics. Returned by `NamedTemporaryFile`."""

    def __init__(self, file, name, delete):
        self._file = file
        self.name = name
        self._delete = delete

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.close()
        return False

    def close(self):
        try:
            self._file.close()
        except Exception:
            pass
        if self._delete:
            try:
                os.remove(self.name)
            except Exception:
                pass

    def write(self, data):
        return self._file.write(data)

    def read(self, *a):
        return self._file.read(*a)

    def flush(self):
        return self._file.flush()

    def seek(self, *a, **k):
        return self._file.seek(*a, **k)


def NamedTemporaryFile(mode="w+b", buffering=-1, encoding=None, newline=None,
                       suffix=None, prefix=None, dir=None, delete=True):
    """Open a uniquely-named temporary file."""
    _path, path = _tempfile.mkstemp(suffix, prefix, dir, "b" not in mode)
    f = open(path, mode)
    return _NamedTempFile(f, path, delete)


class TemporaryDirectory:
    """Create a fresh temp directory; cleans up on `__exit__`."""

    def __init__(self, suffix=None, prefix=None, dir=None):
        self.name = _tempfile.mkdtemp(suffix, prefix, dir)

    def __enter__(self):
        return self.name

    def __exit__(self, *exc):
        self.cleanup()
        return False

    def cleanup(self):
        try:
            import _shutil
            _shutil.rmtree(self.name)
        except Exception:
            pass


__all__ = [
    "gettempdir", "gettempprefix", "mkstemp", "mkdtemp",
    "NamedTemporaryFile", "TemporaryDirectory",
]
