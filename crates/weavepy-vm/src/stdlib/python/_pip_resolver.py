"""``_pip_resolver`` — dependency resolution for the WeavePy pip.

Implements the small surface ``_minipip`` needs to follow
``Requires-Dist`` chains, evaluate PEP 508 markers, walk a wheel's
metadata, and produce a deterministic install plan.

The algorithm is a depth-first walker rather than a SAT solver: pip's
2020+ resolver uses :pep:`517`'s "find any compatible version, then
backtrack on conflicts" semantics; we model the simpler "first
satisfiable version wins" pattern which works for the vast majority of
real-world dependency graphs and only falls over on diamond conflicts.
A conflict raises ``ResolutionError`` so the user sees a clear failure
rather than silently picking the wrong version.
"""

import re
import zipfile

import _packaging
from _packaging import (
    InvalidRequirement,
    Requirement,
    SpecifierSet,
    Version,
    canonicalize_name,
    default_environment,
    wheel_is_compatible,
    wheel_score,
)


class ResolutionError(Exception):
    """Raised when the resolver can't satisfy a request."""


class _ProjectIndex:
    """Adapter wrapping an HTTP-backed PEP 503 index for resolver use."""

    def __init__(self, lookup):
        self._lookup = lookup
        self._cache = {}

    def candidates(self, name: str):
        """Return a list of ``(version, filename, url)`` tuples for ``name``,
        already filtered to compatible wheels and sorted descending by
        version + tag score.
        """
        key = canonicalize_name(name)
        if key in self._cache:
            return self._cache[key]
        raw = self._lookup(name) or []
        seen = []
        for entry in raw:
            filename, url = entry
            if not filename.endswith('.whl'):
                continue
            if not wheel_is_compatible(filename):
                continue
            try:
                parts = filename.split('-')
                version = Version(parts[1])
            except Exception:
                continue
            seen.append((version, filename, url))
        seen.sort(key=lambda t: (t[0], wheel_score(t[1])), reverse=True)
        self._cache[key] = seen
        return seen


def _wheel_metadata(blob: bytes) -> str:
    """Extract the METADATA text from an in-memory wheel."""
    import io
    bio = io.BytesIO(blob)
    with zipfile.ZipFile(bio) as zf:
        for name in zf.namelist():
            if name.endswith('.dist-info/METADATA'):
                with zf.open(name) as f:
                    raw = f.read()
                try:
                    return raw.decode('utf-8')
                except UnicodeDecodeError:
                    return raw.decode('latin-1')
    return ''


def _parse_metadata(text: str) -> dict:
    """RFC 822-shape parse of wheel METADATA: header lines + payload."""
    headers = {}
    multivalued = ('Requires-Dist', 'Provides-Extra', 'Classifier',
                   'Requires-External', 'Project-URL', 'Dynamic')
    for key in multivalued:
        headers[key] = []
    lines = text.split('\n')
    i = 0
    while i < len(lines):
        line = lines[i]
        if not line.strip():
            break
        if ':' not in line:
            i += 1
            continue
        k, _, v = line.partition(':')
        k = k.strip()
        v = v.strip()
        # Continuation lines.
        while i + 1 < len(lines) and lines[i + 1].startswith((' ', '\t')):
            i += 1
            v += '\n' + lines[i].strip()
        if k in multivalued:
            headers[k].append(v)
        else:
            headers[k] = v
        i += 1
    return headers


def _filtered_requires(metadata: dict, extras: set, env: dict) -> list:
    """Walk Requires-Dist lines, applying marker filters and extras."""
    out = []
    for raw in metadata.get('Requires-Dist', ()):
        try:
            req = Requirement(raw)
        except InvalidRequirement:
            continue
        if req.marker is not None:
            # PEP 508 extras are surfaced through the marker `extra ==`
            # construct. Make every extra in turn so the right gates fire.
            applies = False
            envs = [dict(env)]
            for extra in extras or ('',):
                e = dict(env)
                e['extra'] = extra
                envs.append(e)
            for candidate in envs:
                if req.marker.evaluate(candidate):
                    applies = True
                    break
            if not applies:
                continue
        out.append(req)
    return out


class Resolver:
    """Walk a graph of requirements, producing a flat install plan."""

    def __init__(self, downloader, lookup, env=None):
        self.downloader = downloader
        self.lookup = lookup
        self.env = env or default_environment()
        self.index = _ProjectIndex(lookup)
        # name -> (version, filename, url, requirement, extras)
        self.plan = {}

    def resolve(self, requirements):
        """Resolve every requirement in ``requirements`` recursively.

        Returns an ordered list of dicts:
            { 'name', 'version', 'filename', 'url', 'extras' }
        """
        for req in requirements:
            self._resolve_one(req)
        out = []
        for key, entry in self.plan.items():
            out.append({
                'name': key,
                'version': str(entry['version']),
                'filename': entry['filename'],
                'url': entry['url'],
                'extras': sorted(entry['extras']),
            })
        return out

    def _resolve_one(self, req: Requirement):
        if req.marker is not None and not req.marker.evaluate(self.env):
            return
        key = canonicalize_name(req.name)
        if key in self.plan:
            entry = self.plan[key]
            if not req.specifier.contains(entry['version'], prereleases=True):
                raise ResolutionError(
                    'Conflict: already-selected {}=={} does not match {}'.format(
                        req.name, entry['version'], req.specifier))
            entry['extras'].update(req.extras)
            return
        candidates = self.index.candidates(req.name)
        selected = None
        for version, filename, url in candidates:
            if not req.specifier.contains(version, prereleases=True):
                continue
            selected = (version, filename, url)
            break
        if selected is None:
            raise ResolutionError(
                'No compatible distribution found for {}{}'.format(
                    req.name, req.specifier))
        version, filename, url = selected
        self.plan[key] = {
            'name': req.name,
            'version': version,
            'filename': filename,
            'url': url,
            'extras': set(req.extras),
        }
        # Fetch metadata and recurse into Requires-Dist.
        try:
            blob = self.downloader(url)
        except Exception:
            blob = b''
        if not blob:
            return
        text = _wheel_metadata(blob)
        if not text:
            return
        md = _parse_metadata(text)
        for sub in _filtered_requires(md, set(req.extras), self.env):
            self._resolve_one(sub)


# Simple PEP 723 "inline metadata" parser — used by `pip run` to read
# script-embedded dependency declarations. Implemented as a manual
# line scanner because the regex spec is fiddly across re engines.

def parse_pep723(source):
    """Parse PEP 723 inline metadata blocks from a script source."""
    out = {}
    lines = source.splitlines()
    i = 0
    while i < len(lines):
        line = lines[i]
        stripped = line.strip()
        if stripped.startswith('# /// ') and not stripped.endswith('///'):
            # Type name on its own line: ``# /// <name>``.
            kind = stripped[6:].strip()
            collected = []
            i += 1
            while i < len(lines):
                inner = lines[i]
                ins = inner.strip()
                if ins == '# ///':
                    break
                if inner.startswith('# '):
                    collected.append(inner[2:])
                elif inner.startswith('#'):
                    collected.append(inner[1:])
                i += 1
            out[kind] = '\n'.join(collected)
        i += 1
    return out
