"""``importlib.resources`` — read data files bundled inside a package.

Smaller surface than CPython's ``importlib.resources`` (the
multiplexer pattern, ``open_text`` / ``open_binary`` shorthands).
What we ship is enough for the common ``files(package) / 'data.txt'
.read_text()`` idiom that's been the recommended pattern since 3.9.
"""

import os
import sys

from importlib.abc import Traversable


__all__ = [
    'files',
    'as_file',
    'Traversable',
    'Package',
    'Resource',
]


Package = object  # alias for type hints
Resource = str


class _PathTraversable(Traversable):
    """Concrete ``Traversable`` backed by a real filesystem path.

    The future will add an in-memory zip-backed variant for wheels
    distributed as `.whl` files; today every package we ship is
    either a frozen module (no resources) or a directory on disk.
    """

    def __init__(self, path):
        self._path = path

    def __str__(self):
        return self._path

    def __fspath__(self):
        return self._path

    @property
    def name(self):
        return os.path.basename(self._path)

    def iterdir(self):
        for entry in os.listdir(self._path):
            yield _PathTraversable(os.path.join(self._path, entry))

    def read_bytes(self):
        with open(self._path, 'rb') as f:
            return f.read()

    def read_text(self, encoding='utf-8'):
        with open(self._path, 'r', encoding=encoding) as f:
            return f.read()

    def is_dir(self):
        return os.path.isdir(self._path)

    def is_file(self):
        return os.path.isfile(self._path)

    def joinpath(self, *parts):
        return _PathTraversable(os.path.join(self._path, *parts))

    def __truediv__(self, other):
        return self.joinpath(other)


def _package_path(package):
    if isinstance(package, str):
        mod = sys.modules.get(package)
        if mod is None:
            mod = __import__(package)
            for part in package.split('.')[1:]:
                mod = getattr(mod, part)
    else:
        mod = package
    spec = getattr(mod, '__spec__', None)
    if spec is not None and spec.submodule_search_locations:
        return spec.submodule_search_locations[0]
    file = getattr(mod, '__file__', None)
    if file:
        return os.path.dirname(file)
    raise ModuleNotFoundError(
        "no resources available for {!r}".format(getattr(mod, '__name__', mod)))


def files(package):
    return _PathTraversable(_package_path(package))


class _PathFileWrapper:
    """Context-manager returned from :func:`as_file` for the
    common case where the resource is already on disk.
    """

    def __init__(self, path):
        self._path = path

    def __enter__(self):
        return self._path

    def __exit__(self, exc_type, exc_value, tb):
        return False


def as_file(traversable):
    """Yield a real filesystem path for ``traversable``."""
    if isinstance(traversable, _PathTraversable):
        return _PathFileWrapper(traversable._path)
    raise TypeError("as_file expects a Traversable")
