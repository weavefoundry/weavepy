#
# WeavePy: user-visible Pattern / Match objects for the re package.
#
# CPython implements `re.Pattern` and `re.Match` as C types inside the
# `_sre` extension. WeavePy instead keeps `_sre` as a pure-data
# backtracking core (compile + exec returning group spans) and builds
# the user-facing objects here, in Python. Doing so means callable
# `re.sub` replacements, `finditer`, `Scanner`, etc. all run on the
# normal interpreter without the engine ever re-entering the VM.
#
# Behaviour (group semantics, greedy/lazy scanning, empty-match
# handling, split/sub/subn rules) follows CPython 3.13 exactly.

import _sre
from . import _parser
from ._constants import error as PatternError

__all__ = ["Pattern", "Match", "compile_pattern"]

# exec() modes understood by the native core.
_MODE_SEARCH = 0
_MODE_MATCH = 1
_MODE_FULLMATCH = 2

# Cache of parsed replacement templates, keyed by (pattern handle, repl).
# Cleared by re.purge().
_template_cache = {}


def compile_pattern(pattern, flags, code, groups, groupindex, indexgroup):
    """Build a Pattern. Called by re._compiler.compile()."""
    handle = _sre.compile(code, groups)
    return Pattern(handle, pattern, flags, groups, groupindex, indexgroup)


def _clamp_span(string, pos, endpos):
    length = len(string)
    if pos is None:
        pos = 0
    if endpos is None:
        endpos = length
    if pos < 0:
        pos = 0
    elif pos > length:
        pos = length
    if endpos > length:
        endpos = length
    elif endpos < 0:
        endpos = 0
    return pos, endpos


class Pattern:
    __module__ = 're'

    def __init__(self, handle, pattern, flags, groups, groupindex, indexgroup):
        self._handle = handle
        self.pattern = pattern
        self.flags = flags
        self.groups = groups
        self.groupindex = groupindex
        # tuple: group number (1-based) -> name or None
        self._indexgroup = indexgroup

    # -- internal --------------------------------------------------------

    def _exec(self, string, pos, endpos, mode, must_advance):
        return _sre.exec(self._handle, string, pos, endpos, mode,
                         1 if must_advance else 0)

    def _iter(self, string, pos, endpos):
        pos, endpos = _clamp_span(string, pos, endpos)
        must_advance = False
        opos, oendpos = pos, endpos
        while pos <= endpos:
            r = self._exec(string, pos, endpos, _MODE_SEARCH, must_advance)
            if r is None:
                break
            start, end = r[0], r[1]
            yield Match(self, string, opos, oendpos, r)
            must_advance = start == end
            pos = end

    # -- public matching API --------------------------------------------

    def match(self, string, pos=0, endpos=None):
        p, e = _clamp_span(string, pos, endpos)
        r = self._exec(string, p, e, _MODE_MATCH, False)
        if r is None:
            return None
        return Match(self, string, p, e, r)

    def fullmatch(self, string, pos=0, endpos=None):
        p, e = _clamp_span(string, pos, endpos)
        r = self._exec(string, p, e, _MODE_FULLMATCH, False)
        if r is None:
            return None
        return Match(self, string, p, e, r)

    def search(self, string, pos=0, endpos=None):
        p, e = _clamp_span(string, pos, endpos)
        r = self._exec(string, p, e, _MODE_SEARCH, False)
        if r is None:
            return None
        return Match(self, string, p, e, r)

    def findall(self, string, pos=0, endpos=None):
        g = self.groups
        empty = string[:0]
        out = []
        for m in self._iter(string, pos, endpos):
            if g == 0:
                out.append(m.group(0))
            elif g == 1:
                v = m.group(1)
                out.append(v if v is not None else empty)
            else:
                row = []
                for i in range(1, g + 1):
                    v = m.group(i)
                    row.append(v if v is not None else empty)
                out.append(tuple(row))
        return out

    def finditer(self, string, pos=0, endpos=None):
        return self._iter(string, pos, endpos)

    def sub(self, repl, string, count=0):
        return self._subx(repl, string, count)[0]

    def subn(self, repl, string, count=0):
        return self._subx(repl, string, count)

    def _subx(self, repl, string, count):
        if count < 0:
            count = 0
        empty = string[:0]
        if callable(repl):
            filt = repl
        else:
            template = _compile_template(self, repl)
            if len(template) == 1 and not isinstance(template[0], int):
                # pure literal replacement
                literal = template[0]
                filt = lambda m, _l=literal: _l
            else:
                filt = lambda m, _t=template: _expand_template(_t, m)
        out = []
        n = 0
        last = 0
        pos = 0
        endpos = len(string)
        must_advance = False
        while pos <= endpos:
            if count and n >= count:
                break
            r = self._exec(string, pos, endpos, _MODE_SEARCH, must_advance)
            if r is None:
                break
            start, end = r[0], r[1]
            out.append(string[last:start])
            m = Match(self, string, 0, endpos, r)
            out.append(filt(m))
            last = end
            n += 1
            must_advance = start == end
            pos = end
        out.append(string[last:])
        return empty.join(out), n

    def split(self, string, maxsplit=0):
        if maxsplit < 0:
            return [string]
        g = self.groups
        out = []
        n = 0
        last = 0
        pos = 0
        endpos = len(string)
        must_advance = False
        while pos <= endpos:
            if maxsplit and n >= maxsplit:
                break
            r = self._exec(string, pos, endpos, _MODE_SEARCH, must_advance)
            if r is None:
                break
            start, end = r[0], r[1]
            m = Match(self, string, 0, endpos, r)
            out.append(string[last:start])
            for i in range(1, g + 1):
                out.append(m.group(i))
            last = end
            n += 1
            must_advance = start == end
            pos = end
        out.append(string[last:])
        return out

    def scanner(self, string, pos=0, endpos=None):
        return _Scanner(self, string, pos, endpos)

    # -- misc ------------------------------------------------------------

    def __repr__(self):
        s = repr(self.pattern)
        if len(s) > 200:
            s = s[:200]
        # Hide the implicit UNICODE flag (32) the way CPython does.
        flags = self.flags & ~32
        if flags:
            return "re.compile(%s, %s)" % (s, _flags_repr(self.flags))
        return "re.compile(%s)" % s

    def __copy__(self):
        return self

    def __deepcopy__(self, memo):
        return self

    @property
    def groupindex_proxy(self):
        return self.groupindex


# Bit -> name table for Pattern repr (matches CPython's RegexFlag names).
_FLAG_NAMES = [
    (256, 're.ASCII'),
    (2, 're.IGNORECASE'),
    (4, 're.LOCALE'),
    (8, 're.MULTILINE'),
    (16, 're.DOTALL'),
    (64, 're.VERBOSE'),
    (128, 're.DEBUG'),
]


def _flags_repr(flags):
    # Hide the implicit UNICODE flag (32) the way CPython does.
    flags &= ~32
    parts = []
    for bit, name in _FLAG_NAMES:
        if flags & bit:
            parts.append(name)
            flags &= ~bit
    if flags:
        parts.append(hex(flags))
    if not parts:
        return '0'
    return '|'.join(parts)


class Match:
    __module__ = 're'

    def __init__(self, pattern, string, pos, endpos, r):
        self.re = pattern
        self.string = string
        self.pos = pos
        self.endpos = endpos
        self._start = r[0]
        self._end = r[1]
        self._lastindex_raw = r[2]
        self._marks = r[3]

    # -- group span helpers ---------------------------------------------

    def _span_of(self, idx):
        if idx == 0:
            return (self._start, self._end)
        i = (idx - 1) * 2
        return (self._marks[i], self._marks[i + 1])

    def _index(self, group):
        if isinstance(group, int) or (not isinstance(group, str) and hasattr(group, '__index__')):
            idx = int(group)
        else:
            try:
                idx = self.re.groupindex[group]
            except KeyError:
                raise IndexError("no such group") from None
        if not 0 <= idx <= self.re.groups:
            raise IndexError("no such group")
        return idx

    def _getslice(self, idx, default):
        s, e = self._span_of(idx)
        if s < 0 or e < 0:
            return default
        return self.string[s:e]

    # -- public API ------------------------------------------------------

    def group(self, *args):
        if not args:
            return self._getslice(0, None)
        if len(args) == 1:
            return self._getslice(self._index(args[0]), None)
        return tuple(self._getslice(self._index(g), None) for g in args)

    def __getitem__(self, group):
        return self._getslice(self._index(group), None)

    def groups(self, default=None):
        return tuple(self._getslice(i, default)
                     for i in range(1, self.re.groups + 1))

    def groupdict(self, default=None):
        result = {}
        for name, idx in self.re.groupindex.items():
            result[name] = self._getslice(idx, default)
        return result

    def start(self, group=0):
        return self._span_of(self._index(group))[0]

    def end(self, group=0):
        return self._span_of(self._index(group))[1]

    def span(self, group=0):
        return self._span_of(self._index(group))

    @property
    def regs(self):
        spans = [(self._start, self._end)]
        for i in range(1, self.re.groups + 1):
            spans.append(self._span_of(i))
        return tuple(spans)

    @property
    def lastindex(self):
        li = self._lastindex_raw
        return None if li < 0 else li

    @property
    def lastgroup(self):
        li = self.lastindex
        if li is None:
            return None
        try:
            return self.re._indexgroup[li]
        except (IndexError, TypeError):
            return None

    def expand(self, template):
        return _expand_template(_parse_template(self.re, template), self)

    def __copy__(self):
        return self

    def __deepcopy__(self, memo):
        return self

    def __repr__(self):
        text = self.string[self._start:self._end]
        return "<re.Match object; span=(%d, %d), match=%r>" % (
            self._start, self._end, text)


class _Scanner:
    def __init__(self, pattern, string, pos, endpos):
        self.pattern = pattern
        self._string = string
        self._pos, self._endpos = _clamp_span(string, pos, endpos)
        self._opos = self._pos
        self._oendpos = self._endpos
        self._must_advance = False

    def match(self):
        return self._run(_MODE_MATCH)

    def search(self):
        return self._run(_MODE_SEARCH)

    def _run(self, mode):
        if self._pos > self._endpos:
            return None
        r = _sre.exec(self.pattern._handle, self._string, self._pos,
                     self._endpos, mode, 1 if self._must_advance else 0)
        if r is None:
            if mode == _MODE_MATCH:
                return None
            return None
        start, end = r[0], r[1]
        m = Match(self.pattern, self._string, self._opos, self._oendpos, r)
        self._must_advance = start == end
        self._pos = end
        return m


# ---------------------------------------------------------------------------
# Replacement-template handling
# ---------------------------------------------------------------------------

def _parse_template(pattern, repl):
    return _parser.parse_template(repl, pattern)


def _compile_template(pattern, repl):
    key = (pattern._handle, repl)
    try:
        return _template_cache[key]
    except KeyError:
        pass
    template = _parser.parse_template(repl, pattern)
    if len(_template_cache) >= 512:
        _template_cache.clear()
    _template_cache[key] = template
    return template


def _expand_template(template, match):
    # `template` is the flat list returned by _parser.parse_template:
    # literals (str/bytes) interleaved with integer group references.
    empty = match.string[:0]
    parts = []
    for item in template:
        if isinstance(item, int):
            g = match.group(item)
            parts.append(g if g is not None else empty)
        else:
            parts.append(item)
    return empty.join(parts)


def clear_template_cache():
    _template_cache.clear()
