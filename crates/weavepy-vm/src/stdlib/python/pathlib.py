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

    def glob(self, pattern):
        """Yield paths matching ``pattern`` directly under this path.

        Supports the same ``*``, ``?``, ``[...]``, ``**`` wildcards as
        CPython's pathlib; ``**`` is the only wildcard that recurses
        into subdirectories."""
        parts = pattern.split("/")
        yield from _glob_walk(self, parts)

    def rglob(self, pattern):
        """Recursive glob — equivalent to ``glob("**/" + pattern)``."""
        return self.glob("**/" + pattern)

    def walk(self, top_down=True, on_error=None, follow_symlinks=False):
        """PEP 711-style ``Path.walk``: yields ``(self, dirnames,
        filenames)`` for every directory under this path. Mirrors
        ``os.walk`` semantics but produces ``Path`` objects."""
        try:
            entries = list(os.scandir(self._raw))
        except OSError as e:
            if on_error is not None:
                on_error(e)
            entries = []
        dirs = []
        files = []
        for entry in entries:
            if entry.is_dir():
                dirs.append(entry.name)
            else:
                files.append(entry.name)
        if top_down:
            yield (self, dirs, files)
            for d in list(dirs):
                yield from Path(self._raw, d).walk(top_down, on_error, follow_symlinks)
        else:
            for d in list(dirs):
                yield from Path(self._raw, d).walk(top_down, on_error, follow_symlinks)
            yield (self, dirs, files)

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

    def relative_to(self, other, *, walk_up=False):
        """Return ``self`` with ``other`` stripped from the front.

        Mirrors CPython 3.13: raises ``ValueError`` when ``self`` is
        not below ``other`` unless ``walk_up=True``, in which case we
        emit ``..`` components to climb out."""
        other = Path(other) if not isinstance(other, Path) else other
        a_parts = list(self.parts)
        b_parts = list(other.parts)
        if a_parts[: len(b_parts)] == b_parts:
            rest = a_parts[len(b_parts):]
            if not rest:
                return Path(".")
            return Path(*rest)
        if not walk_up:
            raise ValueError(f"{self._raw!r} is not in the subpath of {other._raw!r}")
        common = 0
        while (
            common < len(a_parts)
            and common < len(b_parts)
            and a_parts[common] == b_parts[common]
        ):
            common += 1
        ups = [".."] * (len(b_parts) - common)
        rest = a_parts[common:]
        if not ups and not rest:
            return Path(".")
        return Path(*(ups + rest))


def _glob_walk(base, parts):
    """Pure-Python depth-first glob implementation. Used by
    ``Path.glob`` / ``Path.rglob`` so we don't depend on
    CPython-specific ``_PosixFlavour`` plumbing."""
    import fnmatch

    if not parts:
        return
    head, rest = parts[0], parts[1:]
    if head == "**":
        if not rest:
            for p in _walk_all_under(base):
                yield p
            return
        yield from _glob_walk(base, rest)
        for entry in _list_entries(base):
            full = Path(base._raw, entry.name)
            if entry.is_dir():
                yield from _glob_walk(full, parts)
        return
    matched = []
    for entry in _list_entries(base):
        if fnmatch.fnmatch(entry.name, head):
            matched.append(entry)
    if not rest:
        for entry in matched:
            yield Path(base._raw, entry.name)
        return
    for entry in matched:
        if entry.is_dir():
            yield from _glob_walk(Path(base._raw, entry.name), rest)


def _list_entries(p):
    try:
        return list(os.scandir(p._raw))
    except OSError:
        return []


def _walk_all_under(base):
    for entry in _list_entries(base):
        full = Path(base._raw, entry.name)
        yield full
        if entry.is_dir():
            yield from _walk_all_under(full)
