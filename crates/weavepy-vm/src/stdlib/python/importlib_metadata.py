"""``importlib.metadata`` — query installed distributions.

PEP 566 + PEP 658-shaped metadata access. We walk
``<site>/<dist>-<ver>.dist-info`` (and the older ``.egg-info``
variant) on ``sys.path`` and parse the canonical files.

Surface in this module is the union of what `pip`, `setuptools`,
`pluggy`, `pytest`, and the typical user reach for:

    >>> from importlib.metadata import version, distributions
    >>> version('requests')
    '2.32.3'
    >>> [d.metadata['Name'] for d in distributions()]
    ['pip', 'requests', ...]
"""

import os
import re
import sys

__all__ = [
    'PackageNotFoundError',
    'Distribution',
    'PathDistribution',
    'distributions',
    'distribution',
    'metadata',
    'version',
    'entry_points',
    'EntryPoint',
    'EntryPoints',
    'requires',
    'files',
    'PackagePath',
]


class PackageNotFoundError(ModuleNotFoundError):
    """Raised when ``distribution('name')`` finds nothing."""

    def __str__(self):
        return "No package metadata was found for {!r}".format(self.args[0])

    @property
    def name(self):
        return self.args[0]


_NAME_RE = re.compile(r'^([A-Za-z0-9][A-Za-z0-9._-]*)-(.+)$')


def _normalize(name):
    return re.sub(r'[-_.]+', '-', name).lower()


def _parse_metadata(text):
    """Parse a ``METADATA`` / ``PKG-INFO`` file."""
    headers = {}
    body_lines = []
    in_body = False
    current_key = None
    for line in text.splitlines():
        if in_body:
            body_lines.append(line)
            continue
        if not line.strip():
            in_body = True
            continue
        if line.startswith((' ', '\t')) and current_key is not None:
            headers[current_key] += '\n' + line.strip()
            continue
        if ':' in line:
            k, _, v = line.partition(':')
            current_key = k.strip()
            v = v.strip()
            if current_key in headers:
                if isinstance(headers[current_key], list):
                    headers[current_key].append(v)
                else:
                    headers[current_key] = [headers[current_key], v]
            else:
                headers[current_key] = v
    if body_lines:
        headers['Description'] = '\n'.join(body_lines)
    return headers


def _iter_dist_dirs(path=None):
    """Yield ``(dir_path, kind)`` tuples for every ``.dist-info``
    or ``.egg-info`` directory on the search path. ``kind`` is
    ``'dist-info'`` or ``'egg-info'``.
    """
    if path is None:
        path = sys.path
    seen = set()
    for entry in path:
        if not entry:
            entry = '.'
        try:
            real = os.path.realpath(entry)
        except Exception:
            real = entry
        if real in seen:
            continue
        seen.add(real)
        try:
            names = os.listdir(entry)
        except OSError:
            continue
        for name in names:
            full = os.path.join(entry, name)
            if name.endswith('.dist-info') and os.path.isdir(full):
                yield full, 'dist-info'
            elif name.endswith('.egg-info'):
                yield full, 'egg-info'


class EntryPoint:
    __slots__ = ('name', 'value', 'group', 'dist')

    def __init__(self, name, value, group):
        self.name = name
        self.value = value
        self.group = group
        self.dist = None

    def __repr__(self):
        return 'EntryPoint(name={!r}, value={!r}, group={!r})'.format(
            self.name, self.value, self.group)

    @property
    def module(self):
        return self.value.split(':', 1)[0]

    @property
    def attr(self):
        parts = self.value.split(':', 1)
        return parts[1] if len(parts) == 2 else ''

    def load(self):
        """Resolve ``module:attr`` to the actual callable / object."""
        module_name = self.module
        attr = self.attr
        mod = __import__(module_name, globals(), locals(),
                          [attr] if attr else [], 0)
        if not attr:
            return mod
        target = mod
        for part in attr.split('.'):
            target = getattr(target, part)
        return target

    def matches(self, **params):
        for k, v in params.items():
            if getattr(self, k, None) != v:
                return False
        return True


class EntryPoints(tuple):
    """List-like container with handy filters."""

    def __new__(cls, iterable):
        return tuple.__new__(cls, iterable)

    @property
    def names(self):
        return tuple(ep.name for ep in self)

    @property
    def groups(self):
        return tuple({ep.group for ep in self})

    def select(self, **params):
        return EntryPoints(ep for ep in self if ep.matches(**params))

    def __getitem__(self, key):
        if isinstance(key, str):
            for ep in self:
                if ep.name == key:
                    return ep
            raise KeyError(key)
        return super().__getitem__(key)


class PackagePath(str):
    """A ``str`` subclass that knows how to ``open()`` itself
    relative to a distribution.
    """

    dist = None
    size = None
    hash = None

    def locate(self):
        if self.dist is None:
            return None
        return os.path.join(os.path.dirname(self.dist._path), str(self))

    def read_text(self, encoding='utf-8'):
        with open(self.locate(), encoding=encoding) as f:
            return f.read()

    def read_bytes(self):
        with open(self.locate(), 'rb') as f:
            return f.read()


class Distribution:
    """Base class. Subclasses override ``read_text`` / ``locate_file``
    to map a filename → contents.
    """

    @classmethod
    def from_name(cls, name):
        normalized = _normalize(name)
        for path, _kind in _iter_dist_dirs():
            base = os.path.basename(path)
            m = _NAME_RE.match(base)
            if not m:
                continue
            dist_name = _normalize(m.group(1))
            if dist_name == normalized:
                return PathDistribution(path)
        raise PackageNotFoundError(name)

    @classmethod
    def discover(cls, **kwargs):
        for path, _kind in _iter_dist_dirs(kwargs.get('path')):
            yield PathDistribution(path)

    def read_text(self, filename):
        raise NotImplementedError

    @property
    def metadata(self):
        text = self.read_text('METADATA') or self.read_text('PKG-INFO') or ''
        return _parse_metadata(text)

    @property
    def name(self):
        return self.metadata.get('Name')

    @property
    def version(self):
        return self.metadata.get('Version')

    @property
    def entry_points(self):
        text = self.read_text('entry_points.txt') or ''
        return EntryPoints(self._parse_entry_points(text))

    def _parse_entry_points(self, text):
        group = None
        for line in text.splitlines():
            line = line.strip()
            if not line or line.startswith('#'):
                continue
            if line.startswith('[') and line.endswith(']'):
                group = line[1:-1].strip()
                continue
            if '=' in line and group:
                name, _, value = line.partition('=')
                ep = EntryPoint(name.strip(), value.strip(), group)
                ep.dist = self
                yield ep

    @property
    def files(self):
        records = self.read_text('RECORD')
        if not records:
            return None
        out = []
        for line in records.splitlines():
            if not line.strip():
                continue
            path = line.split(',', 1)[0]
            pp = PackagePath(path)
            pp.dist = self
            out.append(pp)
        return out

    @property
    def requires(self):
        meta = self.metadata
        reqs = meta.get('Requires-Dist')
        if not reqs:
            return []
        if isinstance(reqs, str):
            return [reqs]
        return list(reqs)


class PathDistribution(Distribution):
    def __init__(self, path):
        self._path = path

    def read_text(self, filename):
        full = os.path.join(self._path, filename)
        try:
            with open(full, 'r', encoding='utf-8') as f:
                return f.read()
        except OSError:
            return None

    def locate_file(self, path):
        return os.path.join(os.path.dirname(self._path), path)


def distributions(**kwargs):
    yield from Distribution.discover(**kwargs)


def distribution(name):
    return Distribution.from_name(name)


def metadata(name):
    return distribution(name).metadata


def version(name):
    return distribution(name).version


def entry_points(**params):
    eps = []
    for dist in distributions():
        eps.extend(dist.entry_points)
    eps = EntryPoints(eps)
    if params:
        return eps.select(**params)
    return eps


def requires(name):
    return distribution(name).requires


def files(name):
    return distribution(name).files
