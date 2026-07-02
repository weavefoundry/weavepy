"""``_packaging`` — PEP 440 / 503 / 508 / 425 utilities.

The single home for the package-tooling primitives the rest of the
ecosystem reaches for:

* **PEP 440** — version parsing, normalisation, comparison, specifier
  matching (``~=``, ``==``, ``!=``, ``<=``, ``>=``, ``<``, ``>``, ``===``).
* **PEP 503** — name normalisation for the simple-repository index.
* **PEP 508** — requirement parsing (``foo[bar]>=1.0; python_version >= "3.10"``)
  and environment-marker evaluation.
* **PEP 425** — wheel filename tag parsing and compatibility scoring
  (factored out of ``_minipip`` so non-pip consumers like ``importlib.metadata``
  can share the same matcher).

This module is intentionally dependency-free (only standard ``re``,
``os``, ``sys``, ``platform``) so it can be the *bottom* of the
packaging stack.

The design follows ``pypa/packaging`` closely but reimplements the
surface from scratch — the upstream package is BSD-licensed and ships
as a wheel, which means a circular dependency for the pip bootstrap.
"""

import os
import re
import sys


__all__ = [
    # PEP 440
    'Version',
    'InvalidVersion',
    'SpecifierSet',
    'Specifier',
    'InvalidSpecifier',
    'parse_version',
    # PEP 503
    'canonicalize_name',
    # PEP 508
    'Requirement',
    'InvalidRequirement',
    'Marker',
    'InvalidMarker',
    'default_environment',
    # PEP 425
    'WheelTag',
    'parse_wheel_filename',
    'compatible_tags',
    'wheel_is_compatible',
    'wheel_score',
]


# ============================================================ PEP 503

_CANON_RE = re.compile(r'[-_.]+')


def canonicalize_name(name: str) -> str:
    """PEP 503: normalize a project name for index lookup."""
    return _CANON_RE.sub('-', name).lower()


# ============================================================ PEP 440

class InvalidVersion(ValueError):
    """Raised when a version string can't be parsed as PEP 440."""


_VERSION_PATTERN = r"""
    v?
    (?:
        (?:(?P<epoch>[0-9]+)!)?
        (?P<release>[0-9]+(?:\.[0-9]+)*)
        (?P<pre>
            [-_\.]?
            (?P<pre_l>alpha|a|beta|b|preview|pre|c|rc)
            [-_\.]?
            (?P<pre_n>[0-9]+)?
        )?
        (?P<post>
            (?:-(?P<post_n1>[0-9]+))
            |
            (?:
                [-_\.]?
                (?P<post_l>post|rev|r)
                [-_\.]?
                (?P<post_n2>[0-9]+)?
            )
        )?
        (?P<dev>
            [-_\.]?
            (?P<dev_l>dev)
            [-_\.]?
            (?P<dev_n>[0-9]+)?
        )?
    )
    (?:\+(?P<local>[a-z0-9]+(?:[-_\.][a-z0-9]+)*))?
"""

_VERSION_RE = re.compile(
    r'^\s*' + _VERSION_PATTERN + r'\s*$',
    re.VERBOSE | re.IGNORECASE,
)


def _canonical_pre(label: str) -> str:
    """Normalise pre-release label tokens to {a,b,rc}."""
    label = label.lower()
    if label in ('alpha', 'a'):
        return 'a'
    if label in ('beta', 'b'):
        return 'b'
    if label in ('c', 'rc', 'pre', 'preview'):
        return 'rc'
    return label


def _canonical_post(label):
    if label is None:
        return None
    label = label.lower()
    if label in ('post', 'rev', 'r'):
        return 'post'
    return label


class Version:
    """A parsed PEP 440 version with rich comparison semantics."""

    __slots__ = (
        'epoch', 'release', 'pre', 'post', 'dev', 'local',
        '_original', '_key',
    )

    def __init__(self, version: str):
        if not isinstance(version, str):
            raise InvalidVersion('version must be str, got {!r}'.format(type(version)))
        m = _VERSION_RE.match(version)
        if m is None:
            raise InvalidVersion('Invalid version: {!r}'.format(version))
        self._original = version

        self.epoch = int(m.group('epoch')) if m.group('epoch') else 0
        self.release = tuple(int(p) for p in m.group('release').split('.'))

        if m.group('pre_l'):
            self.pre = (_canonical_pre(m.group('pre_l')),
                        int(m.group('pre_n') or 0))
        else:
            self.pre = None

        if m.group('post_n1') is not None:
            self.post = ('post', int(m.group('post_n1')))
        elif m.group('post_l'):
            self.post = (_canonical_post(m.group('post_l')),
                         int(m.group('post_n2') or 0))
        else:
            self.post = None

        if m.group('dev_l'):
            self.dev = ('dev', int(m.group('dev_n') or 0))
        else:
            self.dev = None

        self.local = m.group('local').lower().replace('_', '.').replace('-', '.') if m.group('local') else None

        self._key = _build_version_key(self)

    @property
    def public(self) -> str:
        """The public-version slice (no local segment)."""
        parts = []
        if self.epoch:
            parts.append('{}!'.format(self.epoch))
        parts.append('.'.join(str(x) for x in self.release))
        if self.pre:
            parts.append('{}{}'.format(*self.pre))
        if self.post:
            parts.append('.post{}'.format(self.post[1]))
        if self.dev:
            parts.append('.dev{}'.format(self.dev[1]))
        return ''.join(parts)

    @property
    def base_version(self) -> str:
        """Version stripped of pre/post/dev/local segments."""
        if self.epoch:
            return '{}!{}'.format(self.epoch, '.'.join(str(x) for x in self.release))
        return '.'.join(str(x) for x in self.release)

    @property
    def is_prerelease(self) -> bool:
        return self.pre is not None or self.dev is not None

    @property
    def is_postrelease(self) -> bool:
        return self.post is not None

    @property
    def is_devrelease(self) -> bool:
        return self.dev is not None

    @property
    def major(self) -> int:
        return self.release[0] if self.release else 0

    @property
    def minor(self) -> int:
        return self.release[1] if len(self.release) >= 2 else 0

    @property
    def micro(self) -> int:
        return self.release[2] if len(self.release) >= 3 else 0

    def __str__(self) -> str:
        parts = []
        if self.epoch:
            parts.append('{}!'.format(self.epoch))
        parts.append('.'.join(str(x) for x in self.release))
        if self.pre:
            parts.append('{}{}'.format(*self.pre))
        if self.post:
            parts.append('.post{}'.format(self.post[1]))
        if self.dev:
            parts.append('.dev{}'.format(self.dev[1]))
        if self.local:
            parts.append('+{}'.format(self.local))
        return ''.join(parts)

    def __repr__(self) -> str:
        return '<Version({!r})>'.format(str(self))

    def __hash__(self) -> int:
        return hash(self._key)

    def __eq__(self, other) -> bool:
        if isinstance(other, str):
            try:
                other = Version(other)
            except InvalidVersion:
                return NotImplemented
        if not isinstance(other, Version):
            return NotImplemented
        return self._key == other._key

    def __ne__(self, other) -> bool:
        eq = self.__eq__(other)
        if eq is NotImplemented:
            return NotImplemented
        return not eq

    def __lt__(self, other) -> bool:
        other = _coerce_version(other)
        if other is None:
            return NotImplemented
        return self._key < other._key

    def __le__(self, other) -> bool:
        other = _coerce_version(other)
        if other is None:
            return NotImplemented
        return self._key <= other._key

    def __gt__(self, other) -> bool:
        other = _coerce_version(other)
        if other is None:
            return NotImplemented
        return self._key > other._key

    def __ge__(self, other) -> bool:
        other = _coerce_version(other)
        if other is None:
            return NotImplemented
        return self._key >= other._key


def _coerce_version(v):
    if isinstance(v, Version):
        return v
    if isinstance(v, str):
        try:
            return Version(v)
        except InvalidVersion:
            return None
    return None


def parse_version(v: str) -> Version:
    return Version(v)


# PEP 440 sort-key construction. We follow the trick `pypa/packaging`
# uses: every key segment is wrapped in a *tuple* whose first element
# is a sortable token (``-inf`` for "missing", numeric/string for real
# values, ``+inf`` for "later than anything") so heterogeneous keys
# always compare against each other under Python's tuple ordering.

_INFINITY = ('z' * 100,)  # Sorts after any real string token.
_NEG_INFINITY = ('',)  # Sorts before any real token.


def _trim_trailing_zeros(release: tuple) -> tuple:
    out = list(release)
    while out and out[-1] == 0:
        out.pop()
    return tuple(out)


def _build_version_key(v):
    release = _trim_trailing_zeros(v.release)

    if v.pre is None and v.post is None and v.dev is not None:
        pre_key = _NEG_INFINITY
    elif v.pre is None:
        if v.post is None and v.dev is None:
            pre_key = _INFINITY
        else:
            pre_key = _NEG_INFINITY
    else:
        # Pre-release labels sort `a < b < rc`. Wrap as a 2-tuple so
        # we can sort against the infinity sentinels.
        pre_key = (v.pre[0], v.pre[1])

    post_key = _NEG_INFINITY if v.post is None else (v.post[0], v.post[1])
    dev_key = _INFINITY if v.dev is None else (v.dev[0], v.dev[1])

    if v.local is None:
        local_key = _NEG_INFINITY
    else:
        local_key = tuple(
            ('', int(part)) if part.isdigit() else (part, 0)
            for part in v.local.split('.')
        )

    return (v.epoch, release, pre_key, post_key, dev_key, local_key)


# ---------------------------------------------------------------------
# PEP 440 specifiers
# ---------------------------------------------------------------------


class InvalidSpecifier(ValueError):
    """Raised when a version specifier can't be parsed."""


_SPECIFIER_OP_RE = re.compile(r'(===|==|!=|~=|<=|>=|<|>)')


class Specifier:
    """One ``op + version`` clause of a :class:`SpecifierSet`."""

    __slots__ = ('op', 'version', '_raw')

    OPERATORS = ('===', '==', '!=', '~=', '<=', '>=', '<', '>')

    def __init__(self, spec: str):
        spec = spec.strip()
        m = _SPECIFIER_OP_RE.match(spec)
        if not m:
            raise InvalidSpecifier('Invalid specifier: {!r}'.format(spec))
        self.op = m.group(1)
        rest = spec[m.end():].strip()
        # Strip optional trailing .* (wildcard) — handled by contains().
        if self.op in ('==', '!='):
            if rest.endswith('.*'):
                self._raw = rest
                self.version = rest[:-2]
                return
        self._raw = rest
        if self.op == '===':
            # Identity match; no parsing.
            self.version = rest
            return
        try:
            self.version = Version(rest)
        except InvalidVersion as exc:
            raise InvalidSpecifier(str(exc)) from None

    def __str__(self) -> str:
        return '{}{}'.format(self.op, self._raw)

    def __repr__(self) -> str:
        return '<Specifier({!r})>'.format(str(self))

    def __eq__(self, other) -> bool:
        if isinstance(other, str):
            try:
                other = Specifier(other)
            except InvalidSpecifier:
                return NotImplemented
        if not isinstance(other, Specifier):
            return NotImplemented
        return self.op == other.op and str(self.version) == str(other.version)

    def __hash__(self) -> int:
        return hash((self.op, str(self.version)))

    def contains(self, version, prereleases: bool = None) -> bool:
        if isinstance(version, str):
            try:
                v = Version(version)
            except InvalidVersion:
                return False
        else:
            v = version

        if self.op == '===':
            return str(v) == str(self._raw)
        if v.is_prerelease and not prereleases:
            # Pre-releases only match if explicit op or version is also
            # a pre-release.
            spec_v = self.version if isinstance(self.version, Version) else None
            if spec_v is None or not spec_v.is_prerelease:
                if self.op not in ('==', '!=') and prereleases is not True:
                    return False

        return self._compare(v)

    def __contains__(self, version) -> bool:
        return self.contains(version)

    def _compare(self, v: Version) -> bool:
        op = self.op
        raw = self._raw
        spec_v = self.version

        if op in ('==', '!=') and raw.endswith('.*'):
            prefix = raw[:-2]
            try:
                pref = Version(prefix)
            except InvalidVersion:
                return False
            actual_release = v.release[: len(pref.release)]
            match = (v.epoch == pref.epoch
                     and actual_release == pref.release)
            return match if op == '==' else not match

        if op == '~=':
            # Compatible release: equivalent to >= V.N, == V.*
            if not isinstance(spec_v, Version):
                return False
            if len(spec_v.release) < 2:
                raise InvalidSpecifier('~= requires release segment, got {!r}'.format(raw))
            upper_release = spec_v.release[:-1]
            upper_release = upper_release[:-1] + (upper_release[-1] + 1,)
            upper = '.'.join(str(x) for x in upper_release)
            try:
                upper_v = Version(upper)
            except InvalidVersion:
                return False
            return spec_v <= v < upper_v

        if not isinstance(spec_v, Version):
            return False
        if op == '==':
            return v.public == spec_v.public
        if op == '!=':
            return v.public != spec_v.public
        if op == '<':
            return v < spec_v
        if op == '<=':
            return v <= spec_v
        if op == '>':
            return v > spec_v
        if op == '>=':
            return v >= spec_v
        return False


class SpecifierSet:
    """A union of version specifiers separated by commas (intersection)."""

    __slots__ = ('specifiers', '_raw')

    def __init__(self, specifiers: str = ''):
        specifiers = (specifiers or '').strip()
        self._raw = specifiers
        if not specifiers:
            self.specifiers = ()
            return
        parts = [p.strip() for p in specifiers.split(',') if p.strip()]
        self.specifiers = tuple(Specifier(p) for p in parts)

    def __str__(self) -> str:
        return ','.join(str(s) for s in self.specifiers)

    def __repr__(self) -> str:
        return '<SpecifierSet({!r})>'.format(str(self))

    def __iter__(self):
        return iter(self.specifiers)

    def __bool__(self) -> bool:
        return bool(self.specifiers)

    def __contains__(self, version) -> bool:
        return self.contains(version)

    def __eq__(self, other) -> bool:
        if isinstance(other, str):
            other = SpecifierSet(other)
        if not isinstance(other, SpecifierSet):
            return NotImplemented
        return frozenset(self.specifiers) == frozenset(other.specifiers)

    def __hash__(self) -> int:
        return hash(frozenset(self.specifiers))

    def contains(self, version, prereleases: bool = None) -> bool:
        if not self.specifiers:
            return True
        if isinstance(version, str):
            try:
                version = Version(version)
            except InvalidVersion:
                return False
        return all(s.contains(version, prereleases=prereleases)
                   for s in self.specifiers)

    def filter(self, iterable, prereleases: bool = None):
        for v in iterable:
            if self.contains(v, prereleases=prereleases):
                yield v


# ============================================================ PEP 508

class InvalidRequirement(ValueError):
    """Raised when a requirement string can't be parsed."""


class InvalidMarker(ValueError):
    """Raised when a marker expression can't be parsed."""


def default_environment() -> dict:
    """Materialise the PEP 508 marker environment for the host."""
    impl_name = sys.implementation.name
    impl_ver = '{}.{}.{}'.format(*sys.version_info[:3])
    try:
        py_full = '{}.{}.{}'.format(*sys.version_info[:3])
    except Exception:
        py_full = '0.0.0'
    py_short = '{}.{}'.format(*sys.version_info[:2])
    if hasattr(os, 'uname'):
        u = os.uname()
        platform_release = u.release
        platform_machine = u.machine
        platform_system = u.sysname
        platform_version = u.version
        platform_node = u.nodename
    else:
        platform_release = ''
        platform_machine = ''
        platform_system = sys.platform
        platform_version = ''
        platform_node = ''
    return {
        'implementation_name': impl_name,
        'implementation_version': impl_ver,
        'os_name': os.name,
        'platform_machine': platform_machine,
        'platform_release': platform_release,
        'platform_system': platform_system,
        'platform_version': platform_version,
        'platform_python_implementation': impl_name.capitalize(),
        'python_full_version': py_full,
        'python_version': py_short,
        'sys_platform': sys.platform,
        'extra': '',
    }


# Tokens accepted by PEP 508 marker grammar.
_MARKER_VARS = frozenset({
    'implementation_name', 'implementation_version',
    'os_name', 'platform_machine', 'platform_release', 'platform_system',
    'platform_version', 'platform_python_implementation',
    'python_full_version', 'python_version',
    'sys_platform', 'extra',
})

_MARKER_OPS = ('<=', '>=', '==', '!=', '~=', '<', '>', 'in', 'not in')


class _MarkerExpr:
    __slots__ = ('left', 'op', 'right')

    def __init__(self, left, op, right):
        self.left = left  # ('var', str) or ('val', str)
        self.op = op
        self.right = right

    def evaluate(self, env):
        l = self._resolve(self.left, env)
        r = self._resolve(self.right, env)
        return _marker_compare(l, self.op, r)

    @staticmethod
    def _resolve(token, env):
        kind, val = token
        if kind == 'var':
            return env.get(val, '')
        return val

    def __repr__(self):
        return '<_MarkerExpr {} {} {}>'.format(self.left, self.op, self.right)


class _MarkerAnd:
    __slots__ = ('parts',)

    def __init__(self, parts):
        self.parts = parts

    def evaluate(self, env):
        return all(p.evaluate(env) for p in self.parts)


class _MarkerOr:
    __slots__ = ('parts',)

    def __init__(self, parts):
        self.parts = parts

    def evaluate(self, env):
        return any(p.evaluate(env) for p in self.parts)


class Marker:
    """A parsed PEP 508 marker expression."""

    __slots__ = ('_raw', '_root')

    def __init__(self, marker: str):
        self._raw = marker
        tokens = _tokenize_marker(marker)
        self._root, idx = _parse_marker_or(tokens, 0)
        if idx != len(tokens):
            raise InvalidMarker('trailing tokens in marker: {!r}'.format(marker))

    def __str__(self):
        return self._raw

    def __repr__(self):
        return '<Marker({!r})>'.format(self._raw)

    def evaluate(self, environment: dict = None) -> bool:
        env = default_environment()
        if environment:
            env.update(environment)
        return self._root.evaluate(env)


def _tokenize_marker(text: str):
    """Tokenize a PEP 508 marker expression."""
    tokens = []
    i = 0
    n = len(text)
    while i < n:
        c = text[i]
        if c.isspace():
            i += 1
            continue
        if c in '()':
            tokens.append((c, c))
            i += 1
            continue
        if c in '"\'':
            quote = c
            j = i + 1
            while j < n and text[j] != quote:
                j += 1
            if j >= n:
                raise InvalidMarker('unterminated string in marker: {!r}'.format(text))
            tokens.append(('val', text[i + 1:j]))
            i = j + 1
            continue
        if c.isalpha() or c == '_':
            j = i
            while j < n and (text[j].isalnum() or text[j] == '_'):
                j += 1
            word = text[i:j]
            wl = word.lower()
            if wl in ('and', 'or'):
                tokens.append((wl, wl))
            elif wl in ('in', 'not'):
                tokens.append((wl, wl))
            else:
                tokens.append(('var', word))
            i = j
            continue
        if c in '<>=!~':
            if i + 1 < n and text[i + 1] == '=':
                tokens.append(('op', text[i:i + 2]))
                i += 2
            elif i + 1 < n and c == '~' and text[i + 1] == '=':
                tokens.append(('op', '~='))
                i += 2
            else:
                tokens.append(('op', c))
                i += 1
            continue
        raise InvalidMarker('unexpected character {!r} in marker {!r}'.format(c, text))
    return tokens


def _parse_marker_or(tokens, idx):
    left, idx = _parse_marker_and(tokens, idx)
    parts = [left]
    while idx < len(tokens) and tokens[idx][0] == 'or':
        idx += 1
        right, idx = _parse_marker_and(tokens, idx)
        parts.append(right)
    if len(parts) == 1:
        return left, idx
    return _MarkerOr(parts), idx


def _parse_marker_and(tokens, idx):
    left, idx = _parse_marker_term(tokens, idx)
    parts = [left]
    while idx < len(tokens) and tokens[idx][0] == 'and':
        idx += 1
        right, idx = _parse_marker_term(tokens, idx)
        parts.append(right)
    if len(parts) == 1:
        return left, idx
    return _MarkerAnd(parts), idx


def _parse_marker_term(tokens, idx):
    if idx >= len(tokens):
        raise InvalidMarker('unexpected end of marker')
    if tokens[idx][0] == '(':
        node, idx = _parse_marker_or(tokens, idx + 1)
        if idx >= len(tokens) or tokens[idx][0] != ')':
            raise InvalidMarker('missing closing paren in marker')
        return node, idx + 1
    left = tokens[idx]
    idx += 1
    if idx >= len(tokens):
        raise InvalidMarker('expected operator after {!r}'.format(left))
    op_tok = tokens[idx]
    if op_tok[0] == 'op':
        op = op_tok[1]
        idx += 1
    elif op_tok[0] == 'in':
        op = 'in'
        idx += 1
    elif op_tok[0] == 'not':
        idx += 1
        if idx >= len(tokens) or tokens[idx][0] != 'in':
            raise InvalidMarker('expected `in` after `not` in marker')
        idx += 1
        op = 'not in'
    else:
        raise InvalidMarker('expected operator at token {!r}'.format(op_tok))
    if idx >= len(tokens):
        raise InvalidMarker('expected right-hand operand')
    right = tokens[idx]
    idx += 1
    return _MarkerExpr(left, op, right), idx


def _marker_compare(l, op, r):
    if op == 'in':
        return str(l) in str(r)
    if op == 'not in':
        return str(l) not in str(r)
    if op in ('==', '!=', '<', '<=', '>', '>='):
        # Try semantic version comparison for python_version-shaped
        # operands; fall back to string comparison.
        try:
            lv = Version(str(l))
            rv = Version(str(r))
            if op == '==':
                return lv == rv
            if op == '!=':
                return lv != rv
            if op == '<':
                return lv < rv
            if op == '<=':
                return lv <= rv
            if op == '>':
                return lv > rv
            if op == '>=':
                return lv >= rv
        except InvalidVersion:
            pass
        sl, sr = str(l), str(r)
        if op == '==':
            return sl == sr
        if op == '!=':
            return sl != sr
        if op == '<':
            return sl < sr
        if op == '<=':
            return sl <= sr
        if op == '>':
            return sl > sr
        if op == '>=':
            return sl >= sr
    raise InvalidMarker('unsupported marker op {!r}'.format(op))


# Requirement parsing — `name[extras] specifier ; marker`.
_REQ_NAME_RE = re.compile(r'[A-Za-z0-9][A-Za-z0-9._-]*')


class Requirement:
    """A parsed PEP 508 requirement."""

    __slots__ = ('name', 'extras', 'specifier', 'url', 'marker', '_raw')

    def __init__(self, requirement_string: str):
        self._raw = requirement_string
        text = requirement_string.strip()
        if not text:
            raise InvalidRequirement('empty requirement')
        m = _REQ_NAME_RE.match(text)
        if m is None:
            raise InvalidRequirement('invalid name in {!r}'.format(text))
        self.name = m.group(0)
        idx = m.end()
        n = len(text)
        # extras
        self.extras = set()
        if idx < n and text[idx] == '[':
            close = text.find(']', idx)
            if close < 0:
                raise InvalidRequirement('unclosed extras in {!r}'.format(text))
            inner = text[idx + 1:close]
            self.extras = {x.strip() for x in inner.split(',') if x.strip()}
            idx = close + 1
        # url
        self.url = None
        if idx < n and text[idx] == '@':
            after_at = text[idx + 1:]
            semi = after_at.find(';')
            if semi < 0:
                self.url = after_at.strip()
                idx = n
            else:
                self.url = after_at[:semi].strip()
                idx = idx + 1 + semi
        # spec ; marker
        rest = text[idx:].strip()
        marker_text = None
        if ';' in rest:
            spec_text, marker_text = rest.split(';', 1)
            spec_text = spec_text.strip()
            marker_text = marker_text.strip()
        else:
            spec_text = rest
        self.specifier = SpecifierSet(spec_text)
        self.marker = Marker(marker_text) if marker_text else None

    def __str__(self):
        parts = [self.name]
        if self.extras:
            parts.append('[{}]'.format(','.join(sorted(self.extras))))
        if self.url:
            parts.append('@ {}'.format(self.url))
        if self.specifier:
            parts.append(str(self.specifier))
        if self.marker:
            parts.append('; {}'.format(self.marker))
        return ''.join(parts)

    def __repr__(self):
        return '<Requirement({!r})>'.format(str(self))

    def applies_to(self, env: dict = None) -> bool:
        """Whether this requirement applies in the given env (PEP 508)."""
        if self.marker is None:
            return True
        return self.marker.evaluate(env)


# ============================================================ PEP 425


class WheelTag:
    """A `(python, abi, platform)` triple parsed from a wheel filename."""

    __slots__ = ('python', 'abi', 'platform')

    def __init__(self, python, abi, plat):
        self.python = python
        self.abi = abi
        self.platform = plat

    def __iter__(self):
        return iter((self.python, self.abi, self.platform))

    def __repr__(self):
        return '<WheelTag {}-{}-{}>'.format(self.python, self.abi, self.platform)


def parse_wheel_filename(name: str):
    """Parse a wheel filename, returning ``(distribution, version, build, tags)``.

    ``tags`` is a list of :class:`WheelTag` covering every combination
    of the dot-separated python/abi/platform tag triples.
    """
    if not name.endswith('.whl'):
        raise ValueError('not a wheel filename: {!r}'.format(name))
    stem = name[:-4]
    parts = stem.split('-')
    if len(parts) < 5:
        raise ValueError('malformed wheel filename: {!r}'.format(name))
    if len(parts) == 5:
        dist, version, py, abi, plat = parts
        build = None
    else:
        dist, version, build, py, abi, plat = parts[0], parts[1], parts[2], parts[-3], parts[-2], parts[-1]
    tags = []
    for p in py.split('.'):
        for a in abi.split('.'):
            for pl in plat.split('.'):
                tags.append(WheelTag(p, a, pl))
    return dist, version, build, tags


def _is_weavepy() -> bool:
    """Whether we're running under WeavePy (vs. vendored on stock CPython)."""
    try:
        return sys.implementation.name == 'weavepy'
    except Exception:
        return False


def compatible_tags():
    """Yield :class:`WheelTag` triples the running interpreter can satisfy.

    The order matches CPython's pip: most specific first, fallback last.

    Because WeavePy mirrors the CPython 3.13 binary ABI (RFC 0043), it
    consumes the *stock* CPython wheel matrix verbatim — the same
    ``cp313`` / ``abi3`` interpreter+ABI tags over the full
    manylinux / macOS / musllinux platform set a real numpy or pandas
    wheel ships. On top of that it accepts an optional **provenance**
    tag (``weavepy``) for wheels a publisher built and verified against
    WeavePy specifically; see below.
    """
    major, minor = sys.version_info[:2]
    plats = _platform_tags()

    # RFC 0047 (wave 5): WeavePy *provenance* tags. A wheel built and
    # verified specifically against WeavePy's mirrored ABI may advertise
    # the `weavepy` interpreter tag (e.g. `pkg-1.0-weavepy-cp313-<plat>.whl`).
    # Such a wheel is invisible to stock CPython (which never emits a
    # `weavepy` tag) yet is the *most preferred* match here — ahead of the
    # generic stock `cp313` wheel it shadows — so a project can ship a
    # WeavePy-blessed build alongside its PyPI artifacts. Emitted only
    # when actually running under WeavePy, keeping this module
    # byte-for-byte CPython-faithful if vendored elsewhere.
    if _is_weavepy():
        for abi in ('cp%d%d' % (major, minor), 'abi3', 'none'):
            for plat in plats:
                yield WheelTag('weavepy', abi, plat)
        yield WheelTag('weavepy', 'none', 'any')

    pys = [
        'cp%d%d' % (major, minor),
        'cp%d' % major,
        'py%d%d' % (major, minor),
        'py%d' % major,
        'py3', 'py2.py3',
    ]
    abis = ['cp%d%d' % (major, minor), 'abi3', 'none']
    for py in pys:
        for abi in abis:
            for plat in plats:
                yield WheelTag(py, abi, plat)
    # `py3-none-any` etc. always work for pure-Python wheels.
    for py in pys:
        yield WheelTag(py, 'none', 'any')


def _platform_tags(plat=None, machine=None):
    """Platform compatibility tags for ``plat``/``machine`` (defaulting to
    the running host). The parameters exist for testability — the matrix
    is otherwise host-derived."""
    out = ['any']
    if plat is None:
        plat = sys.platform
    if machine is None:
        machine = os.uname().machine if hasattr(os, 'uname') else 'x86_64'
    if not machine:
        machine = 'x86_64'
    if plat == 'darwin':
        for major in range(10, 16):
            for minor in range(0, 17):
                out.append('macosx_%d_%d_universal2' % (major, minor))
                out.append('macosx_%d_%d_x86_64' % (major, minor))
                out.append('macosx_%d_%d_arm64' % (major, minor))
    elif plat.startswith('linux'):
        out.append('linux_%s' % machine)
        out.append('manylinux1_%s' % machine)
        out.append('manylinux2010_%s' % machine)
        out.append('manylinux2014_%s' % machine)
        for minor in range(17, 40):
            out.append('manylinux_2_%d_%s' % (minor, machine))
        # PEP 656 musllinux (Alpine / musl libc). numpy and pandas both
        # publish `musllinux_1_1` and `musllinux_1_2` wheels next to their
        # manylinux ones; omitting these makes the resolver skip every
        # binary wheel on a musl host. `musllinux_${maj}_${min}` is keyed
        # on the musl ABI version (currently 1.2), so we span 1_0..1_5 for
        # forward headroom, mirroring the manylinux range above.
        for minor in range(0, 6):
            out.append('musllinux_1_%d_%s' % (minor, machine))
    elif plat == 'win32':
        out.append('win_amd64')
        out.append('win32')
        out.append('win_arm64')
    return out


def wheel_is_compatible(filename: str) -> bool:
    """Whether ``filename`` is installable on the running interpreter."""
    try:
        _, _, _, tags = parse_wheel_filename(filename)
    except ValueError:
        return False
    accept = set()
    for t in compatible_tags():
        accept.add((t.python, t.abi, t.platform))
    return any((t.python, t.abi, t.platform) in accept for t in tags)


def wheel_score(filename: str) -> int:
    """Return a priority score; higher = more preferred.

    Mirrors pip's tie-breaking: prefer cp-tagged wheels over abi3 over
    none, prefer arch-specific platform tags over `any`. A WeavePy
    *provenance* wheel (interpreter tag ``weavepy``, RFC 0047) outranks
    the generic stock build it shadows — it is only ever a candidate when
    running under WeavePy (``compatible_tags`` gates it), so the boost
    never perturbs selection elsewhere.
    """
    try:
        _, _, _, tags = parse_wheel_filename(filename)
    except ValueError:
        return -1
    best = 0
    for t in tags:
        s = 0
        if t.python == 'weavepy':
            s += 16
        elif t.python.startswith('cp'):
            s += 8
        if t.abi != 'none':
            s += 4
        if t.platform != 'any':
            s += 2
        if t.abi == 'abi3':
            s += 1
        best = max(best, s)
    return best


# ============================================================ Self-test

if __name__ == '__main__':
    v = Version('1.4.0.post1')
    assert str(v) == '1.4.0.post1', v
    assert Version('1.4.0') < Version('1.4.1')
    assert Version('1.4.0a1') < Version('1.4.0')
    assert Version('1.0') == Version('1.0.0')
    assert Version('2!1.0') > Version('1.99')
    s = SpecifierSet('>=1.0,<2.0')
    assert s.contains('1.5')
    assert not s.contains('2.0')
    assert SpecifierSet('==1.4.*').contains('1.4.99')
    assert not SpecifierSet('==1.4.*').contains('1.5.0')
    assert SpecifierSet('~=2.2').contains('2.5.0')
    assert not SpecifierSet('~=2.2').contains('3.0.0')
    r = Requirement('numpy[fast]>=1.20; python_version >= "3.10"')
    assert r.name == 'numpy'
    assert r.extras == {'fast'}
    assert r.specifier.contains('1.21')
    assert r.applies_to(default_environment())
    print('packaging self-test OK')
