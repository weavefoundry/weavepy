"""WeavePy's pure-Python ``pathlib`` lite.

Implements the ``Path`` class with the most-used surface area: path
joining, attribute accessors (``name``, ``stem``, ``suffix``,
``parent``, ``parts``), reading and writing text and bytes, and basic
filesystem queries (``exists``, ``is_file``, ``is_dir``, ``iterdir``).
"""

import os

__all__ = ["PurePath", "Path"]


_SEP = os.path.sep


class PurePath:
    """Path object that doesn't perform any I/O."""

    def __init__(self, *segments):
        parts = []
        for segment in segments:
            if isinstance(segment, PurePath):
                parts.append(segment._raw)
            else:
                parts.append(str(segment))
        if not parts:
            self._raw = ""
        elif len(parts) == 1:
            self._raw = parts[0]
        else:
            self._raw = os.path.join(*parts)

    def __str__(self):
        return self._raw

    def __repr__(self):
        return type(self).__name__ + "(" + repr(self._raw) + ")"

    def __eq__(self, other):
        if isinstance(other, PurePath):
            return self._raw == other._raw
        return NotImplemented

    def __hash__(self):
        return hash(self._raw)

    def __fspath__(self):
        return self._raw

    def __truediv__(self, other):
        return type(self)(self._raw, other)

    def __rtruediv__(self, other):
        return type(self)(other, self._raw)

    @property
    def parts(self):
        if not self._raw:
            return ()
        parts = []
        head, tail = os.path.split(self._raw)
        while tail:
            parts.append(tail)
            head, tail = os.path.split(head)
        if head:
            parts.append(head)
        parts.reverse()
        return tuple(parts)

    @property
    def name(self):
        return os.path.basename(self._raw)

    @property
    def stem(self):
        base = self.name
        root, _ = os.path.splitext(base)
        return root

    @property
    def suffix(self):
        _, ext = os.path.splitext(self.name)
        return ext

    @property
    def suffixes(self):
        name = self.name
        if not name or name.startswith("."):
            return []
        rest = name
        out = []
        while True:
            stem, ext = os.path.splitext(rest)
            if not ext:
                break
            out.insert(0, ext)
            rest = stem
        return out

    @property
    def parent(self):
        parent_str = os.path.dirname(self._raw)
        return type(self)(parent_str)

    @property
    def parents(self):
        result = []
        current = self.parent
        while str(current) != str(current.parent):
            result.append(current)
            current = current.parent
        result.append(current)
        return result

    def with_name(self, name):
        return type(self)(os.path.join(os.path.dirname(self._raw), name))

    def with_suffix(self, suffix):
        if suffix and not suffix.startswith("."):
            raise ValueError("suffix must start with '.'")
        base, _ = os.path.splitext(self._raw)
        return type(self)(base + suffix)

    def joinpath(self, *segments):
        return type(self)(self._raw, *segments)

    def is_absolute(self):
        return os.path.isabs(self._raw)

    def as_posix(self):
        return self._raw.replace(os.path.sep, "/")


class Path(PurePath):
    """Concrete path that performs filesystem operations."""

    def exists(self):
        return os.path.exists(self._raw)

    def is_file(self):
        return os.path.isfile(self._raw)

    def is_dir(self):
        return os.path.isdir(self._raw)

    def absolute(self):
        return Path(os.path.abspath(self._raw))

    def resolve(self, strict=False):
        return Path(os.path.realpath(self._raw))

    def read_text(self, encoding=None, errors=None):
        with open(self._raw, "r") as f:
            return f.read()

    def write_text(self, data, encoding=None, errors=None):
        with open(self._raw, "w") as f:
            return f.write(data)

    def read_bytes(self):
        with open(self._raw, "rb") as f:
            return f.read()

    def write_bytes(self, data):
        with open(self._raw, "wb") as f:
            return f.write(data)

    def iterdir(self):
        for name in os.listdir(self._raw):
            yield Path(self._raw, name)

    def mkdir(self, mode=0o777, parents=False, exist_ok=False):
        if parents:
            os.makedirs(self._raw, exist_ok=exist_ok)
        else:
            try:
                os.mkdir(self._raw)
            except FileExistsError:
                if not exist_ok:
                    raise

    def unlink(self, missing_ok=False):
        try:
            os.remove(self._raw)
        except FileNotFoundError:
            if not missing_ok:
                raise

    def open(self, mode="r", encoding=None, errors=None):
        return open(self._raw, mode)

    @classmethod
    def cwd(cls):
        return cls(os.getcwd())

    @classmethod
    def home(cls):
        return cls(os.path.expanduser("~"))
