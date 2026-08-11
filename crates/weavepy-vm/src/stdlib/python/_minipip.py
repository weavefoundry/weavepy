"""``_minipip`` — a pip-compatible installer for WeavePy.

Implements pip's CLI surface against PyPI (the real `https://pypi.org/`
index) plus arbitrary PEP 503 simple indexes. Sub-commands::

    pip install <wheel-file>                   # local install
    pip install <sdist.tar.gz>                 # local PEP 517 build
    pip install --no-binary :all: <name>       # force source build
    pip install <name>[op<version>][extras]    # resolve + install
    pip install -r requirements.txt
    pip install -e <path>                      # editable / sdist install
    pip uninstall <package> [-y]
    pip list  [--format {columns,freeze,json}]
    pip show <package>
    pip freeze
    pip download <name>                        # download wheel only
    pip wheel <name>                           # build/download wheel
    pip cache  {list,purge,info}
    pip check                                  # consistency check
    pip --version

The PEP 440 specifier matcher (``foo>=1.0,<2.0``), the PEP 508 marker
evaluator (``foo; python_version >= "3.10"``), and the dependency
resolver live in :mod:`_packaging` and :mod:`_pip_resolver`. Source-
distribution builds delegate to :func:`_install_sdist` which drives
the in-tree :mod:`_pep517` build backend.
"""

import argparse
import hashlib
import io
import json
import os
import re
import shutil
import sys
import tempfile
import zipfile
from urllib import request as urlrequest
from urllib.parse import urljoin

import _packaging
from _packaging import (
    InvalidRequirement,
    Requirement,
    SpecifierSet,
    Version,
    canonicalize_name,
    default_environment,
    parse_wheel_filename,
    wheel_is_compatible,
    wheel_score,
)


__all__ = ['main']

VERSION = '24.0.0+weavepy'

# Real pip exposes `pip.__version__`; tooling probes it (pandas'
# `show_versions` raises "Can't determine version for pip" without it).
__version__ = VERSION
DEFAULT_INDEX = 'https://pypi.org/simple/'
USER_AGENT = 'weavepy-pip/{}'.format(VERSION)


def _site_packages():
    """Pick the install destination. Mirrors pip's
    ``--prefix`` fallback chain: VIRTUAL_ENV → sys.prefix.
    """
    venv = os.environ.get('VIRTUAL_ENV')
    base = venv or sys.prefix
    py = 'python%d.%d' % sys.version_info[:2]
    if os.name == 'nt':
        return os.path.join(base, 'Lib', 'site-packages')
    return os.path.join(base, 'lib', py, 'site-packages')


def _bin_dir():
    base = os.environ.get('VIRTUAL_ENV') or sys.prefix
    return os.path.join(base, 'Scripts' if os.name == 'nt' else 'bin')


# --------------------------------------------------------------------- HTTP

def _http_get(url):
    """Fetch ``url``; return bytes."""
    req = urlrequest.Request(url, headers={'User-Agent': USER_AGENT,
                                              'Accept': 'application/json'})
    with urlrequest.urlopen(req) as resp:
        return resp.read()


def _http_text(url):
    return _http_get(url).decode('utf-8', errors='replace')


# --------------------------------------------------------------------- PEP 503 simple repo

_LINK_RE = re.compile(
    r'<a [^>]*href=["\']([^"\']+)["\'][^>]*>([^<]+)</a>',
    re.IGNORECASE)


def _normalize(name):
    return re.sub(r'[-_.]+', '-', name).lower()


def _list_distributions(name, index_url):
    """Yield every distribution on a PEP 503 simple index for ``name``.

    Returns a list of ``(filename, url)`` tuples (wheels *and* sdists).
    The caller is responsible for filtering by compatibility.
    """
    if not index_url.endswith('/'):
        index_url += '/'
    project_url = urljoin(index_url, _normalize(name) + '/')
    try:
        html = _http_text(project_url)
    except Exception:
        return []
    out = []
    for href, label in _LINK_RE.findall(html):
        url = href.split('#', 1)[0]
        if not url.startswith('http'):
            url = urljoin(project_url, url)
        out.append((label, url))
    return out


def _find_wheel_on_index(name, index_url, python_version=None):
    """Look up ``name`` on a PEP 503 simple index, return the URL of
    the best-matching pure-Python wheel.
    """
    candidates = []
    for label, url in _list_distributions(name, index_url):
        if not label.endswith('.whl'):
            continue
        if not _is_compatible_wheel(label):
            continue
        try:
            version = parse_wheel_filename(label)[1]
        except ValueError:
            version = _wheel_version(label)
        candidates.append((version, label, url))
    if not candidates:
        return None, None
    candidates.sort(
        key=lambda t: (_version_key(t[0]), _wheel_tag_score(t[1])),
        reverse=True,
    )
    _, label, url = candidates[0]
    return label, url


def _find_sdist_on_index(name, index_url, specifier=None):
    """Return the highest-version sdist URL for ``name`` (or ``(None,
    None)``), honouring an optional version *specifier* (RFC 0062:
    `--no-binary` rows pin exact versions)."""
    candidates = []
    for label, url in _list_distributions(name, index_url):
        lower = label.lower()
        if not (lower.endswith('.tar.gz') or lower.endswith('.zip')
                or lower.endswith('.tgz')):
            continue
        # Strip prefix `name-` and extension.
        norm = _normalize(name) + '-'
        head = _normalize(label)
        if not head.startswith(norm):
            continue
        tail = label[len(norm):]
        for ext in ('.tar.gz', '.tgz', '.zip'):
            if tail.lower().endswith(ext):
                version = tail[:-len(ext)]
                break
        else:
            version = tail
        if specifier is not None:
            try:
                if not specifier.contains(version):
                    continue
            except Exception:
                continue
        candidates.append((version, label, url))
    if not candidates:
        return None, None
    candidates.sort(key=lambda t: _version_key(t[0]), reverse=True)
    _, label, url = candidates[0]
    return label, url


def _wheel_version(filename):
    """Pull the version out of a wheel filename."""
    parts = filename.split('-')
    return parts[1] if len(parts) > 1 else '0'


def _version_key(v):
    """Cheap version sort key: split on `.` / non-numeric chunks and
    coerce each piece to an int when possible. Pre-releases (dev/a/b/rc
    segments) sort *below* every final release so a bare install never
    picks `1.0.dev3` over `0.28.1` (pip's default, PEP 440).
    """
    is_pre = bool(re.search(r'(?:^|[.+-])(?:dev|a|b|c|rc|alpha|beta|pre|preview)\d*',
                            v, re.IGNORECASE))
    out = [0 if is_pre else 1]
    for chunk in re.split(r'[.+-]', v):
        m = re.match(r'(\d+)', chunk)
        out.append(int(m.group(1)) if m else 0)
    return tuple(out)


def _compatible_python_tags():
    """The CPython tags WeavePy claims to be ABI-compatible with.
    A wheel built for any of these is accepted.

    We claim compatibility with the WeavePy major.minor (which mirrors
    a CPython release we target) — extensions targeting that tag are
    loadable since our `Python.h` reproduces the public API surface.
    """
    major, minor = sys.version_info[:2]
    tags = [
        'py3',
        'py%d' % major,
        'py%d%d' % (major, minor),
        'py2.py3',
        'cp%d' % major,
        'cp%d%d' % (major, minor),
    ]
    return tags


def _compatible_abi_tags():
    """ABI tags this WeavePy binary can satisfy. `none` always works
    (pure Python). `abi3` is the stable-ABI flavour that CPython 3.x
    extensions can be compiled with — we support it because our
    `Python.h` exports the stable subset.

    `cp3X` (e.g. `cp313`) is the per-version full ABI that CPython
    builds default to; we accept it because WeavePy mirrors the
    target CPython's ABI byte-for-byte.
    """
    major, minor = sys.version_info[:2]
    return ['none', 'abi3', 'cp%d%d' % (major, minor)]


def _compatible_platform_tags():
    """Platform tags this WeavePy binary can run.

    `any` always works (pure Python). Platform-specific wheels are
    accepted for the running OS/arch. We deliberately match a broad
    family of glibc / macOS / Windows tags so wheel resolution
    works without forcing every wheel to be tagged exactly for
    `manylinux_2_28_aarch64` or similar — pip's normal fallback
    behaviour.
    """
    tags = ['any']
    platform = sys.platform
    machine = os.uname().machine if hasattr(os, 'uname') else 'x86_64'
    if platform == 'darwin':
        # Universal2 plus the *host* arch only (macOS 10.9..15 family) —
        # a foreign-arch wheel would pass resolution and then dlopen-fail.
        arch = machine if machine in ('arm64', 'x86_64') else 'x86_64'
        for ver in (10, 11, 12, 13, 14, 15):
            for sub in range(0, 16):
                tags.append('macosx_%d_%d_universal2' % (ver, sub))
                tags.append('macosx_%d_%d_%s' % (ver, sub, arch))
    elif platform.startswith('linux'):
        # manylinux2014 / manylinux_2_xx / linux_<arch>.
        suffix = machine if machine else 'x86_64'
        tags.append('linux_%s' % suffix)
        tags.append('manylinux1_%s' % suffix)
        tags.append('manylinux2010_%s' % suffix)
        tags.append('manylinux2014_%s' % suffix)
        for ver in range(17, 40):
            tags.append('manylinux_2_%d_%s' % (ver, suffix))
    elif platform == 'win32':
        tags.append('win_amd64')
        tags.append('win32')
        tags.append('win_arm64')
    return tags


def _is_compatible_wheel(filename):
    """PEP 425 wheel-tag compatibility check.

    We honour the standard `python-abi-platform` triple and accept a
    wheel if every component matches one of our compatible tags. The
    matching is multi-tag aware: a single wheel filename can carry
    several dot-separated python/abi/platform tags, and the wheel is
    accepted if *any* combination is compatible.
    """
    stem = filename[:-4]  # strip ``.whl``
    parts = stem.split('-')
    if len(parts) < 5:
        return False
    py_tag = parts[-3]
    abi_tag = parts[-2]
    plat_tag = parts[-1]

    py_ok = any(p in _compatible_python_tags() for p in py_tag.split('.'))
    abi_ok = any(a in _compatible_abi_tags() for a in abi_tag.split('.'))
    plat_ok = any(p in _compatible_platform_tags() for p in plat_tag.split('.'))
    # PEP 425 stable-ABI backwards series: an abi3 wheel is tagged with
    # the *oldest* CPython it supports (`cp37-abi3-…` runs on 3.7+), so
    # the `cp3k`+`abi3` pairing is compatible for any k <= our minor.
    # Only the pair — `cp37-none-any` stays rejected, matching pip.
    if not (py_ok and abi_ok) and 'abi3' in abi_tag.split('.'):
        minor = sys.version_info[1]
        for p in py_tag.split('.'):
            if p.startswith('cp3') and p[3:].isdigit() and int(p[3:]) <= minor:
                py_ok = abi_ok = True
                break
    return py_ok and abi_ok and plat_ok


def _wheel_tag_score(filename):
    """Cheap preference ordering: prefer wheels that match more
    specifically (i.e. exact ABI / platform over `any` / `none`)
    so users don't accidentally get a sdist-fallback when a real
    binary is available.
    """
    stem = filename[:-4]
    parts = stem.split('-')
    if len(parts) < 5:
        return 0
    score = 0
    py_tag = parts[-3]
    abi_tag = parts[-2]
    plat_tag = parts[-1]
    if 'cp' in py_tag:
        score += 4
    if abi_tag != 'none':
        score += 2
    if plat_tag != 'any':
        score += 1
    return score


# --------------------------------------------------------------------- wheel install

_EXT_SUFFIXES = ('.so', '.dylib', '.pyd')


def _is_extension_module(name):
    return any(name.endswith(s) for s in _EXT_SUFFIXES)


def _install_wheel(wheel_path, *, dest=None, scheme='purelib'):
    """Unpack ``wheel_path`` into ``dest`` (default site-packages).
    Returns the list of installed files.

    Handles both pure-Python wheels and binary wheels carrying
    ``.so``/``.dylib``/``.pyd`` extension modules. The wheel `.data/`
    layout is honoured: ``scripts`` go to the bin dir, ``platlib``
    payloads are merged into site-packages alongside ``purelib``.
    """
    if dest is None:
        dest = _site_packages()
    os.makedirs(dest, exist_ok=True)
    installed = []
    scripts_dir = _bin_dir()
    data_prefix = None
    with zipfile.ZipFile(wheel_path) as zf:
        data_prefix = _data_prefix(zf)
        for name in zf.namelist():
            if name.endswith('/'):
                continue
            target = os.path.join(dest, name)
            section = None
            if data_prefix and name.startswith(data_prefix):
                rel = name[len(data_prefix):]
                section, _, payload = rel.partition('/')
                if section == 'scripts':
                    target = os.path.join(scripts_dir, payload)
                elif section in ('purelib', 'platlib'):
                    target = os.path.join(dest, payload)
                elif section == 'headers':
                    target = os.path.join(
                        os.environ.get('VIRTUAL_ENV') or sys.prefix,
                        'include',
                        payload,
                    )
                elif section == 'data':
                    target = os.path.join(
                        os.environ.get('VIRTUAL_ENV') or sys.prefix,
                        payload,
                    )
                else:
                    # Unknown section: drop the file rather than
                    # littering site-packages with a `.data/foo/`
                    # ghost path.
                    continue
            target_dir = os.path.dirname(target)
            if target_dir:
                os.makedirs(target_dir, exist_ok=True)
            with zf.open(name) as src, open(target, 'wb') as dst:
                shutil.copyfileobj(src, dst)
            installed.append(target)
            if section == 'scripts' or _is_extension_module(name):
                try:
                    os.chmod(target, 0o755)
                except OSError:
                    pass
    # RFC 0055 WS2 — generate `[console_scripts]` launchers, the way
    # real pip does (this is how `pip`, `pytest`, `flask`, … CLIs
    # appear in a venv's bin dir).
    installed.extend(_generate_console_scripts(installed, scripts_dir))
    return installed


_SCRIPT_TEMPLATE = """\
#!{python}
# -*- coding: utf-8 -*-
import re
import sys
from {module} import {import_name}
if __name__ == '__main__':
    sys.argv[0] = re.sub(r'(-script\\.pyw|\\.exe)?$', '', sys.argv[0])
    sys.exit({call}())
"""


def _generate_console_scripts(installed, scripts_dir):
    """Write launcher scripts for every ``[console_scripts]`` entry in
    the just-installed dist-info. Returns the created paths."""
    entry_points = [p for p in installed
                    if p.replace(os.sep, '/').endswith('.dist-info/entry_points.txt')]
    created = []
    for ep_path in entry_points:
        try:
            with open(ep_path, 'r', encoding='utf-8') as f:
                lines = f.read().splitlines()
        except OSError:
            continue
        section = None
        for line in lines:
            line = line.strip()
            if not line or line.startswith(('#', ';')):
                continue
            if line.startswith('[') and line.endswith(']'):
                section = line[1:-1].strip()
                continue
            if section != 'console_scripts' or '=' not in line:
                continue
            script_name, _, spec = line.partition('=')
            script_name = script_name.strip()
            spec = spec.split('[', 1)[0].strip()  # drop [extras]
            module, _, attr = spec.partition(':')
            module = module.strip()
            attr = attr.strip() or 'main'
            import_name = attr.split('.', 1)[0]
            target = os.path.join(scripts_dir, script_name)
            body = _SCRIPT_TEMPLATE.format(
                python=sys.executable, module=module,
                import_name=import_name, call=attr)
            try:
                os.makedirs(scripts_dir, exist_ok=True)
                with open(target, 'w', encoding='utf-8') as f:
                    f.write(body)
                os.chmod(target, 0o755)
            except OSError:
                continue
            created.append(target)
    return created


def _data_prefix(zf):
    for name in zf.namelist():
        if '.data/' in name:
            return name.split('.data/')[0] + '.data/'
    return '___never_matches___/'


# --------------------------------------------------------------------- commands

def _parse_no_binary(values):
    """Parse pip's ``--no-binary`` syntax into ``(all, names)``.

    Values may repeat and each value may be `:all:` or a
    comma-separated package-name list (RFC 0062 WS2).
    """
    force_all = False
    names = set()
    for value in values or []:
        for part in value.split(','):
            part = part.strip()
            if not part:
                continue
            if part == ':all:':
                force_all = True
            elif part == ':none:':
                force_all = False
                names.clear()
            else:
                names.add(canonicalize_name(part))
    return force_all, names


def _spec_forces_sdist(spec, no_binary_all, no_binary_names):
    if no_binary_all:
        return True
    if not no_binary_names:
        return False
    try:
        name = Requirement(spec).name
    except InvalidRequirement:
        name = re.split(r'[<>=!~ ]', spec, maxsplit=1)[0].strip()
    return canonicalize_name(name) in no_binary_names


def _is_local_sdist(path):
    return os.path.isfile(path) and path.lower().endswith(
        ('.tar.gz', '.tgz', '.zip'))


def cmd_install(args):
    """``pip install ...``."""
    targets = list(args.packages or [])
    if args.requirement:
        for r in args.requirement:
            targets.extend(_read_requirements(r))
    if not targets:
        print('ERROR: no packages specified', file=sys.stderr)
        return 1
    dest = args.target
    if getattr(args, 'user', False) and dest is None:
        import site
        dest = site.getusersitepackages()
    if getattr(args, 'root', None):
        # `--root` re-anchors the destination under an alternate root
        # (ensurepip passes it for altinstall trees).
        base = dest or _site_packages()
        dest = os.path.join(args.root, os.path.relpath(base, os.sep))
    no_binary_all, no_binary_names = _parse_no_binary(
        getattr(args, 'no_binary', []))
    # Local sdist files install directly through the PEP 517 driver
    # in every mode (`pip install ./pkg-1.0.tar.gz`, RFC 0062 WS2).
    rc = 0
    remaining = []
    for spec in targets:
        if _is_local_sdist(spec):
            if not args.quiet:
                print('Installing sdist: {}'.format(spec))
            try:
                _install_sdist(spec, dest=dest)
            except Exception as exc:
                print('ERROR: {}: {}'.format(spec, exc), file=sys.stderr)
                rc = 1
        else:
            remaining.append(spec)
    targets = remaining
    if not targets:
        return rc
    if getattr(args, 'no_index', False):
        # Offline mode: satisfy every target from `--find-links`
        # directories (`ensurepip`'s bootstrap path). Wheels, plus
        # sdists for `--no-binary` targets (RFC 0062 WS2: the C-sdist
        # proof rows run fully offline from the wheel cache).
        # Requires-Dist chains are resolved against the local cache
        # unless --no-deps: `install --no-index --find-links D requests`
        # must pull urllib3/idna/certifi/… exactly like online pip.
        plan = []
        wheel_targets = []
        for spec in targets:
            if _spec_forces_sdist(spec, no_binary_all, no_binary_names):
                sdist = _find_sdist_in_links(spec, args.find_links)
                if sdist is None:
                    print('ERROR: no matching sdist for {!r} in {}'.format(
                        spec, args.find_links), file=sys.stderr)
                    rc = 1
                    continue
                if not args.quiet:
                    print('Building sdist: {}'.format(sdist))
                try:
                    _install_sdist(sdist, dest=dest)
                except Exception as exc:
                    print('ERROR: {}: {}'.format(spec, exc), file=sys.stderr)
                    rc = 1
                continue
            wheel_targets.append(spec)
        # One shared resolve for every wheel target, so a direct pin
        # (`pytest==8.3.5`) wins over another target's transitive
        # Requires-Dist on the same project.
        if wheel_targets:
            try:
                plan.extend(
                    _resolve_from_links(wheel_targets, args.find_links,
                                        follow_deps=not args.no_deps))
            except RuntimeError as exc:
                print('ERROR: {}'.format(exc), file=sys.stderr)
                rc = 1
        seen = set()
        for wheel in plan:
            if wheel in seen:
                continue
            seen.add(wheel)
            if not args.quiet:
                print('Installing wheel: {}'.format(wheel))
            _install_wheel(wheel, dest=dest)
        return rc
    args.target = dest
    # Online `--no-binary` targets skip the wheel resolver and take
    # the index-sdist path directly (dependencies of a forced-sdist
    # target are not followed — the callers that need this pin their
    # requirement lists explicitly).
    if no_binary_all or no_binary_names:
        remaining = []
        for spec in targets:
            if _spec_forces_sdist(spec, no_binary_all, no_binary_names):
                try:
                    _install_sdist_spec(spec, index_url=args.index_url,
                                        quiet=args.quiet, dest=args.target)
                except Exception as exc:
                    print('ERROR: {}: {}'.format(spec, exc), file=sys.stderr)
                    rc = 1
            else:
                remaining.append(spec)
        targets = remaining
        if not targets:
            return rc
    if args.no_deps:
        # Old behaviour: install each spec individually.
        for spec in targets:
            try:
                _install_spec(spec, index_url=args.index_url,
                              quiet=args.quiet, dest=args.target,
                              allow_sdist=not args.only_binary)
            except Exception as exc:
                print('ERROR: {}: {}'.format(spec, exc), file=sys.stderr)
                rc = 1
        return rc
    try:
        _install_with_resolver(targets, index_url=args.index_url,
                               quiet=args.quiet, dest=args.target,
                               dry_run=args.dry_run,
                               allow_sdist=not args.only_binary)
    except Exception as exc:
        print('ERROR: {}'.format(exc), file=sys.stderr)
        rc = 1
    return rc


def _find_wheel_in_links(spec, link_dirs):
    """Resolve *spec* to a wheel file inside the `--find-links` dirs.

    Highest version wins; environment tags are matched with the same
    PEP 425 matcher the index path uses.
    """
    if os.path.isfile(spec) and spec.endswith('.whl'):
        return spec
    try:
        req = Requirement(spec)
        name = req.name
        specifier = req.specifier
    except InvalidRequirement:
        name = re.split(r'[<>=!~ ]', spec, maxsplit=1)[0].strip()
        specifier = None
    canonical = canonicalize_name(name)
    best = None
    best_key = None
    for d in link_dirs or []:
        try:
            entries = os.listdir(d)
        except OSError:
            continue
        for entry in entries:
            if not entry.endswith('.whl'):
                continue
            try:
                whl_name, whl_version, _build, _tags = parse_wheel_filename(entry)
            except Exception:
                continue
            if canonicalize_name(whl_name) != canonical:
                continue
            if not wheel_is_compatible(entry):
                continue
            if specifier is not None and not specifier.contains(str(whl_version)):
                continue
            key = (Version(str(whl_version)), wheel_score(entry))
            if best_key is None or key > best_key:
                best = os.path.join(d, entry)
                best_key = key
    return best


def _find_sdist_in_links(spec, link_dirs):
    """Resolve *spec* to an sdist file inside the ``--find-links`` dirs
    (RFC 0062 WS2: `--no-binary` targets in offline mode). Highest
    version wins.
    """
    if _is_local_sdist(spec):
        return spec
    try:
        req = Requirement(spec)
        name = req.name
        specifier = req.specifier
    except InvalidRequirement:
        name = re.split(r'[<>=!~ ]', spec, maxsplit=1)[0].strip()
        specifier = None
    canonical = canonicalize_name(name)
    best = None
    best_key = None
    for d in link_dirs or []:
        try:
            entries = os.listdir(d)
        except OSError:
            continue
        for entry in entries:
            lower = entry.lower()
            for ext in ('.tar.gz', '.tgz', '.zip'):
                if lower.endswith(ext):
                    stem = entry[:-len(ext)]
                    break
            else:
                continue
            # sdists are named `{name}-{version}`; the name half may
            # use any PEP 503-equivalent spelling.
            head, sep, version = stem.rpartition('-')
            if not sep or canonicalize_name(head) != canonical:
                continue
            try:
                key = Version(version)
            except Exception:
                continue
            if specifier is not None and not specifier.contains(version):
                continue
            if best_key is None or key > best_key:
                best = os.path.join(d, entry)
                best_key = key
    return best


def _sdist_suffix(label):
    """The full archive suffix of an sdist filename (`.tar.gz` must
    survive intact; `os.path.splitext` would truncate it to `.gz`)."""
    lower = label.lower()
    for ext in ('.tar.gz', '.tgz', '.zip'):
        if lower.endswith(ext):
            return ext
    return '.tar.gz'


def _install_sdist_spec(spec, *, index_url, quiet=False, dest=None):
    """Install *spec* from its index sdist, never a wheel
    (`--no-binary`, RFC 0062 WS2)."""
    specifier = None
    try:
        req = Requirement(spec)
        name = req.name
        specifier = req.specifier
    except InvalidRequirement:
        name = re.split(r'[<>=!~ ]', spec, maxsplit=1)[0].strip()
    label, url = _find_sdist_on_index(name, index_url, specifier)
    if url is None:
        raise RuntimeError('no sdist found for {!r}'.format(name))
    if not quiet:
        print('Downloading sdist {}'.format(label))
    blob = _http_get(url)
    with tempfile.NamedTemporaryFile(
            suffix=_sdist_suffix(label), delete=False) as tmp:
        tmp.write(blob)
        tmp_path = tmp.name
    try:
        _install_sdist(tmp_path, dest=dest)
    finally:
        try:
            os.remove(tmp_path)
        except OSError:
            pass


def _resolve_from_links(specs, link_dirs, *, follow_deps=True):
    """Resolve *specs* (a list of direct requirement strings — and,
    when *follow_deps*, their Requires-Dist closures) against the
    ``--find-links`` directories.

    Direct requirements are resolved *first* and their picks win for
    the whole plan: a transitive ``Requires-Dist`` for the same
    project must reuse the directly-pinned wheel instead of
    re-resolving to the newest cached version (pip parity — installing
    ``pytest==8.3.5 pytest-cov`` from a cache that also holds a newer
    pytest must not let pytest-cov's ``pytest>=7`` dep overwrite the
    pin; RFC 0062 WS4 hit exactly this with the selftest test-dep
    sets).

    Returns wheel paths in dependency-first order so imports work the
    moment each wheel lands. Raises ``RuntimeError`` when a needed
    wheel is missing from the cache.
    """
    if isinstance(specs, str):
        specs = [specs]
    ordered = []
    done = set()
    pinned = {}

    def parse(spec):
        try:
            req = Requirement(spec)
            return canonicalize_name(req.name), set(req.extras)
        except InvalidRequirement:
            name = re.split(r'[<>=!~ ;]', spec, maxsplit=1)[0].strip()
            return canonicalize_name(name), set()

    direct = []
    for spec in specs:
        key, extras = parse(spec)
        wheel = _find_wheel_in_links(spec, link_dirs)
        if wheel is None:
            raise RuntimeError(
                'no matching wheel for {!r} in {}'.format(spec, link_dirs))
        pinned.setdefault(key, wheel)
        direct.append((spec, extras))

    def visit(spec, extras, chain):
        key, req_extras = parse(spec)
        if key in chain:  # dependency cycle (rare but legal)
            return
        if key in done:
            return
        wheel = pinned.get(key)
        if wheel is None:
            wheel = _find_wheel_in_links(spec, link_dirs)
        if wheel is None:
            raise RuntimeError(
                'no matching wheel for {!r} in {}'.format(spec, link_dirs))
        if follow_deps:
            env = default_environment()
            for raw in _wheel_requires_dist(wheel):
                try:
                    dep = Requirement(raw)
                except InvalidRequirement:
                    continue
                if dep.marker is not None:
                    wanted = req_extras | extras
                    # A dep guarded by `extra == "…"` only applies when
                    # that extra was requested.
                    if not any(
                        dep.marker.evaluate(dict(env, extra=e))
                        for e in (wanted or {''})
                    ):
                        continue
                visit(str(dep), set(dep.extras), chain | {key})
        done.add(key)
        ordered.append(wheel)

    for spec, extras in direct:
        visit(spec, extras, frozenset())
    return ordered


def _wheel_requires_dist(wheel_path):
    """The ``Requires-Dist:`` lines of a wheel's METADATA."""
    try:
        with zipfile.ZipFile(wheel_path) as zf:
            meta_name = next(
                (n for n in zf.namelist()
                 if n.endswith('.dist-info/METADATA') and n.count('/') == 1),
                None)
            if meta_name is None:
                return []
            text = zf.read(meta_name).decode('utf-8', errors='replace')
    except (OSError, zipfile.BadZipFile):
        return []
    out = []
    for line in text.split('\n\n', 1)[0].splitlines():
        if line.startswith('Requires-Dist:'):
            out.append(line.split(':', 1)[1].strip())
    return out


def _read_requirements(path):
    out = []
    with open(path, 'r', encoding='utf-8') as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith('#') or line.startswith('-'):
                continue
            out.append(line)
    return out


def _install_with_resolver(specs, *, index_url, quiet=False, dest=None,
                            dry_run=False, allow_sdist=True):
    """Resolve dependencies then install in dependency order."""
    try:
        import _pip_resolver
    except ImportError:
        # Should never happen — bundled module.
        return _install_each(specs, index_url=index_url, quiet=quiet,
                             dest=dest, allow_sdist=allow_sdist)
    # Split specs into local wheels (no resolution) and remote names.
    local = []
    remote = []
    for s in specs:
        if os.path.isfile(s) and s.endswith('.whl'):
            local.append(s)
        else:
            remote.append(s)
    if local:
        for path in local:
            if not quiet:
                print('Installing wheel: {}'.format(path))
            if not dry_run:
                _install_wheel(path, dest=dest)
    if not remote:
        return
    reqs = []
    for s in remote:
        try:
            reqs.append(Requirement(s))
        except InvalidRequirement as exc:
            raise RuntimeError('invalid requirement {!r}: {}'.format(s, exc))
    downloader = lambda url: _http_get(url)
    lookup = lambda name: _list_distributions(name, index_url)
    resolver = _pip_resolver.Resolver(downloader, lookup)
    plan = resolver.resolve(reqs)
    if not quiet:
        print('Resolved {} package(s):'.format(len(plan)))
        for entry in plan:
            print('  {}-{}'.format(entry['name'], entry['version']))
    if dry_run:
        return
    for entry in plan:
        if not quiet:
            print('Downloading {}'.format(entry['filename']))
        blob = _http_get(entry['url'])
        with tempfile.NamedTemporaryFile(suffix='.whl', delete=False) as tmp:
            tmp.write(blob)
            tmp_path = tmp.name
        try:
            _install_wheel(tmp_path, dest=dest)
        finally:
            try:
                os.remove(tmp_path)
            except OSError:
                pass


def _install_each(specs, *, index_url, quiet=False, dest=None,
                  allow_sdist=True):
    """Fallback installer that doesn't follow dependencies."""
    for spec in specs:
        _install_spec(spec, index_url=index_url, quiet=quiet,
                      dest=dest, allow_sdist=allow_sdist)


def _install_spec(spec, *, index_url, quiet=False, dest=None,
                  allow_sdist=True):
    """Install one requirement specifier."""
    if os.path.isfile(spec) and spec.endswith('.whl'):
        if not quiet:
            print('Installing wheel: {}'.format(spec))
        _install_wheel(spec, dest=dest)
        return
    try:
        req = Requirement(spec)
        name = req.name
    except InvalidRequirement:
        name = re.split(r'[<>=!~ ]', spec, maxsplit=1)[0].strip()
    if not quiet:
        print('Looking up {} on {}'.format(name, index_url))
    label, url = _find_wheel_on_index(name, index_url)
    if url is None:
        if allow_sdist:
            label, url = _find_sdist_on_index(name, index_url)
            if url is None:
                raise RuntimeError(
                    'no compatible wheel or sdist found for {!r}'.format(name))
            if not quiet:
                print('Downloading sdist {}'.format(label))
            blob = _http_get(url)
            with tempfile.NamedTemporaryFile(suffix=_sdist_suffix(label),
                                             delete=False) as tmp:
                tmp.write(blob)
                tmp_path = tmp.name
            try:
                _install_sdist(tmp_path, dest=dest)
            finally:
                try:
                    os.remove(tmp_path)
                except OSError:
                    pass
            return
        raise RuntimeError('no compatible wheel found for {!r}'.format(name))
    if not quiet:
        print('Downloading {}'.format(label))
    blob = _http_get(url)
    with tempfile.NamedTemporaryFile(suffix='.whl', delete=False) as tmp:
        tmp.write(blob)
        tmp_path = tmp.name
    try:
        _install_wheel(tmp_path, dest=dest)
    finally:
        try:
            os.remove(tmp_path)
        except OSError:
            pass


def _install_sdist(sdist_path, *, dest=None):
    """Build an sdist into a wheel via PEP 517 and install it."""
    try:
        import _pep517
    except ImportError:
        raise RuntimeError('sdist install requires the _pep517 backend')
    extracted = _pep517.extract_sdist(sdist_path)
    try:
        wheel_path = _pep517.build_wheel(extracted)
        if wheel_path is None:
            raise RuntimeError('PEP 517 build produced no wheel')
        _install_wheel(wheel_path, dest=dest)
    finally:
        try:
            shutil.rmtree(extracted, ignore_errors=True)
        except OSError:
            pass


def cmd_uninstall(args):
    """``pip uninstall ...``.

    Best-effort: removes the ``.dist-info`` directory and the files
    listed in its ``RECORD``. Doesn't run any pre-uninstall scripts.
    """
    site = _site_packages()
    rc = 0
    for name in args.packages:
        info = _find_dist_info(site, name)
        if info is None:
            print('No package {!r} found'.format(name), file=sys.stderr)
            rc = 1
            continue
        if not args.yes:
            ans = input('Uninstall {}? [y/N] '.format(name)).strip().lower()
            if ans != 'y':
                continue
        record = os.path.join(info, 'RECORD')
        try:
            with open(record, 'r', encoding='utf-8') as f:
                for line in f:
                    rel = line.split(',', 1)[0]
                    if not rel:
                        continue
                    target = os.path.normpath(os.path.join(site, rel))
                    try:
                        os.remove(target)
                    except OSError:
                        pass
        except OSError:
            pass
        try:
            shutil.rmtree(info)
        except OSError:
            pass
        # Real pip prints `Successfully uninstalled <name>-<version>`;
        # `test_venv.do_test_with_pip` greps for the prefix.
        dist = os.path.basename(info)[:-len('.dist-info')]
        print('Successfully uninstalled {}'.format(dist))
    return rc


def _find_dist_info(site, name):
    if not os.path.isdir(site):
        return None
    normalized = _normalize(name)
    for entry in os.listdir(site):
        if entry.endswith('.dist-info'):
            base = entry[:-len('.dist-info')]
            base_name = base.rsplit('-', 1)[0]
            if _normalize(base_name) == normalized:
                return os.path.join(site, entry)
    return None


def cmd_list(args):
    site = _site_packages()
    if not os.path.isdir(site):
        return 0
    rows = []
    for entry in sorted(os.listdir(site)):
        if entry.endswith('.dist-info'):
            base = entry[:-len('.dist-info')]
            try:
                name, version = base.rsplit('-', 1)
            except ValueError:
                continue
            rows.append((name, version))
    fmt = getattr(args, 'format', 'columns')
    if fmt == 'json':
        out = [{'name': n, 'version': v} for n, v in rows]
        print(json.dumps(out, indent=2))
        return 0
    if fmt == 'freeze':
        for name, version in rows:
            print('{}=={}'.format(name, version))
        return 0
    width = max((len(n) for n, _ in rows), default=10)
    for name, version in rows:
        print('{name:<{w}}  {version}'.format(name=name, version=version, w=width))
    return 0


def cmd_show(args):
    site = _site_packages()
    for name in args.packages:
        info = _find_dist_info(site, name)
        if info is None:
            print('{}: not installed'.format(name))
            continue
        try:
            with open(os.path.join(info, 'METADATA'), 'r',
                        encoding='utf-8') as f:
                text = f.read()
        except OSError:
            text = ''
        print(text.split('\n\n', 1)[0])
        print('Location: {}'.format(site))
        print()
    return 0


def cmd_freeze(args):
    """``pip freeze`` — emit installed packages as a requirements file."""
    site = _site_packages()
    if not os.path.isdir(site):
        return 0
    rows = []
    for entry in sorted(os.listdir(site)):
        if entry.endswith('.dist-info'):
            base = entry[:-len('.dist-info')]
            try:
                name, version = base.rsplit('-', 1)
            except ValueError:
                continue
            rows.append((name, version))
    for name, version in rows:
        print('{}=={}'.format(name, version))
    return 0


def cmd_download(args):
    """``pip download <name>`` — fetch the wheel without installing."""
    dest = args.dest or os.getcwd()
    os.makedirs(dest, exist_ok=True)
    rc = 0
    for spec in args.packages:
        try:
            req = Requirement(spec)
            name = req.name
        except InvalidRequirement:
            name = spec
        label, url = _find_wheel_on_index(name, args.index_url)
        if url is None:
            print('ERROR: no compatible wheel for {!r}'.format(name),
                  file=sys.stderr)
            rc = 1
            continue
        if not args.quiet:
            print('Downloading {} -> {}'.format(label, dest))
        blob = _http_get(url)
        with open(os.path.join(dest, label), 'wb') as f:
            f.write(blob)
    return rc


def cmd_wheel(args):
    """``pip wheel <name>`` — alias for download for now."""
    return cmd_download(args)


def cmd_cache(args):
    """``pip cache {info,list,purge}`` — operate on the local cache."""
    cache_dir = _cache_dir()
    if args.cache_cmd == 'info' or args.cache_cmd is None:
        print('Cache location: {}'.format(cache_dir))
        if os.path.isdir(cache_dir):
            n = sum(1 for _ in os.listdir(cache_dir))
            print('Cached entries: {}'.format(n))
        return 0
    if args.cache_cmd == 'list':
        if os.path.isdir(cache_dir):
            for entry in sorted(os.listdir(cache_dir)):
                print(entry)
        return 0
    if args.cache_cmd == 'purge':
        if os.path.isdir(cache_dir):
            for entry in os.listdir(cache_dir):
                try:
                    p = os.path.join(cache_dir, entry)
                    if os.path.isdir(p):
                        shutil.rmtree(p, ignore_errors=True)
                    else:
                        os.remove(p)
                except OSError:
                    pass
        print('Cache purged')
        return 0
    return 1


def _cache_dir():
    base = os.environ.get('XDG_CACHE_HOME')
    if base:
        return os.path.join(base, 'weavepy-pip')
    home = os.path.expanduser('~')
    if sys.platform == 'darwin':
        return os.path.join(home, 'Library', 'Caches', 'weavepy-pip')
    if os.name == 'nt':
        return os.path.join(os.environ.get('LOCALAPPDATA', home),
                            'weavepy-pip', 'Cache')
    return os.path.join(home, '.cache', 'weavepy-pip')


def cmd_check(args):
    """``pip check`` — verify the install satisfies its declared dependencies."""
    site = _site_packages()
    if not os.path.isdir(site):
        print('No packages installed.')
        return 0
    installed = {}
    for entry in sorted(os.listdir(site)):
        if entry.endswith('.dist-info'):
            base = entry[:-len('.dist-info')]
            try:
                name, version = base.rsplit('-', 1)
            except ValueError:
                continue
            installed[canonicalize_name(name)] = version
    problems = []
    env = default_environment()
    for entry in sorted(os.listdir(site)):
        if not entry.endswith('.dist-info'):
            continue
        meta_path = os.path.join(site, entry, 'METADATA')
        try:
            with open(meta_path, 'r', encoding='utf-8') as f:
                text = f.read()
        except OSError:
            continue
        my_name = entry[:-len('.dist-info')].rsplit('-', 1)[0]
        for line in text.splitlines():
            if not line.startswith('Requires-Dist:'):
                continue
            raw = line.split(':', 1)[1].strip()
            try:
                req = Requirement(raw)
            except InvalidRequirement:
                continue
            if req.marker and not req.marker.evaluate(env):
                continue
            installed_version = installed.get(canonicalize_name(req.name))
            if installed_version is None:
                problems.append('{} requires {} (missing)'.format(my_name, raw))
                continue
            if not req.specifier.contains(installed_version, prereleases=True):
                problems.append('{} requires {} but {} is installed'.format(
                    my_name, raw, installed_version))
    if not problems:
        print('No broken requirements found.')
        return 0
    for p in problems:
        print(p)
    return 1


def cmd_config(args):
    """``pip config`` — minimal config shim (no-op stub)."""
    print('No config keys set.')
    return 0


def cmd_search(args):
    """``pip search`` — deprecated in upstream pip; we accept and warn."""
    print('pip search has been disabled (returns no results).',
          file=sys.stderr)
    return 0


def main(argv=None):
    """``python -m pip``."""
    if argv is None:
        argv = sys.argv[1:]
    parser = argparse.ArgumentParser(prog='pip', description=__doc__)
    parser.add_argument('--version', action='store_true')
    subs = parser.add_subparsers(dest='command')

    install = subs.add_parser('install', help='install a package')
    install.add_argument('packages', nargs='*')
    install.add_argument('-r', '--requirement', action='append', default=[])
    install.add_argument('--index-url', default=DEFAULT_INDEX)
    install.add_argument('-q', '--quiet', action='store_true')
    install.add_argument('-v', '--verbose', action='count', default=0)
    install.add_argument('--no-deps', action='store_true',
                         help="don't follow Requires-Dist chains")
    install.add_argument('--dry-run', action='store_true',
                         help='resolve only; don\'t install')
    install.add_argument('--only-binary', action='store_true',
                         help='reject sdists (don\'t try PEP 517 builds)')
    # RFC 0062 WS2 — force source builds. Accepts pip's syntax:
    # `:all:` or a comma-separated package-name list; may repeat.
    install.add_argument('--no-binary', action='append', default=[],
                         metavar='NAMES',
                         help='force sdist builds for NAMES (or :all:)')
    # Accepted for real-pip CLI compatibility; the in-tree PEP 517
    # driver never isolates builds (backends import from the live
    # environment), so this is already the behavior.
    install.add_argument('--no-build-isolation', action='store_true',
                         help='(no-op: builds are never isolated)')
    install.add_argument('-t', '--target', default=None,
                         help='install into the given directory')
    install.add_argument('-e', '--editable', action='append', default=[],
                         help='install in editable mode (best-effort)')
    install.add_argument('-U', '--upgrade', action='store_true')
    # RFC 0055 WS2 — the offline surface `ensurepip` drives
    # (`install --no-cache-dir --no-index --find-links <dir> pip`).
    install.add_argument('--no-index', action='store_true',
                         help='ignore the package index; only --find-links')
    install.add_argument('--find-links', action='append', default=[],
                         metavar='DIR',
                         help='look for wheel archives in DIR')
    install.add_argument('--no-cache-dir', action='store_true',
                         help='disable the download cache')
    install.add_argument('--root', default=None,
                         help='install relative to this alternate root')
    install.add_argument('--user', action='store_true',
                         help='install into the user site-packages')
    install.set_defaults(func=cmd_install)

    uninstall = subs.add_parser('uninstall', help='remove a package')
    uninstall.add_argument('packages', nargs='+')
    uninstall.add_argument('-y', '--yes', action='store_true')
    uninstall.add_argument('-v', '--verbose', action='count', default=0)
    # Accepted for CPython `ensurepip._uninstall` compatibility; the
    # facade never phones home for version checks anyway.
    uninstall.add_argument('--disable-pip-version-check',
                           action='store_true')
    uninstall.set_defaults(func=cmd_uninstall)

    list_cmd = subs.add_parser('list', help='list installed packages')
    list_cmd.add_argument('--format', default='columns',
                          choices=('columns', 'freeze', 'json'))
    list_cmd.set_defaults(func=cmd_list)

    show = subs.add_parser('show', help='show package metadata')
    show.add_argument('packages', nargs='+')
    show.set_defaults(func=cmd_show)

    freeze = subs.add_parser('freeze', help='dump installed package list')
    freeze.set_defaults(func=cmd_freeze)

    download = subs.add_parser('download', help='download a wheel')
    download.add_argument('packages', nargs='+')
    download.add_argument('-d', '--dest', default=None)
    download.add_argument('--index-url', default=DEFAULT_INDEX)
    download.add_argument('-q', '--quiet', action='store_true')
    download.set_defaults(func=cmd_download)

    wheel = subs.add_parser('wheel', help='build wheels (alias for download)')
    wheel.add_argument('packages', nargs='+')
    wheel.add_argument('-d', '--dest', default=None)
    wheel.add_argument('--index-url', default=DEFAULT_INDEX)
    wheel.add_argument('-q', '--quiet', action='store_true')
    wheel.set_defaults(func=cmd_wheel)

    cache = subs.add_parser('cache', help='inspect or purge the cache')
    cache.add_argument('cache_cmd', nargs='?',
                       choices=('info', 'list', 'purge'))
    cache.set_defaults(func=cmd_cache)

    check = subs.add_parser('check', help='check installed deps')
    check.set_defaults(func=cmd_check)

    config = subs.add_parser('config', help='no-op config shim')
    config.set_defaults(func=cmd_config)

    search = subs.add_parser('search', help='deprecated; returns nothing')
    search.add_argument('terms', nargs='*')
    search.set_defaults(func=cmd_search)

    opts = parser.parse_args(argv)
    if opts.version:
        # Real pip's shape: `pip <ver> from <location> (python <X.Y>)`.
        # `test_venv.EnsurePipTest` greps the venv path out of it.
        location = os.path.dirname(os.path.abspath(
            globals().get('__file__') or '.'))
        print('pip {} from {} (python {}.{})'.format(
            VERSION, location, *sys.version_info[:2]))
        return 0
    if not getattr(opts, 'command', None):
        parser.print_help()
        return 1
    return opts.func(opts)


if __name__ == '__main__':
    sys.exit(main())
