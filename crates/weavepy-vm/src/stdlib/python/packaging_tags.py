"""``packaging.tags`` — PEP 425 wheel tag matching."""

from _packaging import (
    WheelTag as _BaseTag,
    compatible_tags as _compat_tags,
)


__all__ = [
    'Tag', 'compatible_tags', 'sys_tags', 'cpython_tags',
    'parse_tag', 'INTERPRETER_SHORT_NAMES',
]


INTERPRETER_SHORT_NAMES = {
    'cpython': 'cp',
    'pypy': 'pp',
    'graalpy': 'gp',
    'weavepy': 'cp',  # WeavePy claims CPython ABI compatibility.
}


class Tag:
    """A `(interpreter, abi, platform)` triple, hashable + comparable."""

    __slots__ = ('_interpreter', '_abi', '_platform', '_hash')

    def __init__(self, interpreter: str, abi: str, platform: str):
        self._interpreter = interpreter.lower()
        self._abi = abi.lower()
        self._platform = platform.lower()
        self._hash = hash((self._interpreter, self._abi, self._platform))

    @property
    def interpreter(self):
        return self._interpreter

    @property
    def abi(self):
        return self._abi

    @property
    def platform(self):
        return self._platform

    def __hash__(self):
        return self._hash

    def __eq__(self, other):
        if not isinstance(other, Tag):
            return NotImplemented
        return (self._interpreter == other._interpreter
                and self._abi == other._abi
                and self._platform == other._platform)

    def __repr__(self):
        return '<Tag {!r}>'.format(str(self))

    def __str__(self):
        return '{}-{}-{}'.format(self._interpreter, self._abi, self._platform)


def parse_tag(tag: str):
    """Split a possibly-dotted tag string into a set of Tag objects."""
    pys, abis, plats = tag.split('-')
    out = set()
    for p in pys.split('.'):
        for a in abis.split('.'):
            for pl in plats.split('.'):
                out.add(Tag(p, a, pl))
    return out


def compatible_tags(python_version=None, interpreter=None, abis=None,
                     platforms=None):
    """Mirror :func:`_packaging.compatible_tags` but yield `Tag`."""
    for t in _compat_tags():
        yield Tag(t.python, t.abi, t.platform)


def cpython_tags(python_version=None, abis=None, platforms=None):
    """Yield only CPython-shaped tags from the running interpreter."""
    for t in compatible_tags():
        if t.interpreter.startswith('cp'):
            yield t


def sys_tags():
    """Yield every compatible tag for the running interpreter."""
    yield from compatible_tags()
