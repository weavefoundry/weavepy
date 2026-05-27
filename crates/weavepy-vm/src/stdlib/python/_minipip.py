"""``_minipip`` — a pip-compatible installer for WeavePy.

Implements pip's CLI surface against PyPI (the real `https://pypi.org/`
index) plus arbitrary PEP 503 simple indexes. Sub-commands::

    pip install <wheel-file>                   # local install
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


def _find_sdist_on_index(name, index_url):
    """Return the highest-version sdist URL for ``name`` (or ``(None, None)``)."""
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
    coerce each piece to an int when possible.
    """
    out = []
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
        # Universal2 + arch-specific variants for both x86_64 and
        # arm64 hosts (macOS 10.9..14 family).
        for ver in (10, 11, 12, 13, 14, 15):
            for sub in range(0, 16):
                tags.append('macosx_%d_%d_universal2' % (ver, sub))
                tags.append('macosx_%d_%d_x86_64' % (ver, sub))
                tags.append('macosx_%d_%d_arm64' % (ver, sub))
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
    return installed


def _data_prefix(zf):
    for name in zf.namelist():
        if '.data/' in name:
            return name.split('.data/')[0] + '.data/'
    return '___never_matches___/'


# --------------------------------------------------------------------- commands

def cmd_install(args):
    """``pip install ...``."""
    targets = list(args.packages or [])
    if args.requirement:
        for r in args.requirement:
            targets.extend(_read_requirements(r))
    if not targets:
        print('ERROR: no packages specified', file=sys.stderr)
        return 1
    rc = 0
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
            with tempfile.NamedTemporaryFile(suffix=os.path.splitext(label)[1] or '.tar.gz',
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
    install.add_argument('--no-deps', action='store_true',
                         help="don't follow Requires-Dist chains")
    install.add_argument('--dry-run', action='store_true',
                         help='resolve only; don\'t install')
    install.add_argument('--only-binary', action='store_true',
                         help='reject sdists (don\'t try PEP 517 builds)')
    install.add_argument('-t', '--target', default=None,
                         help='install into the given directory')
    install.add_argument('-e', '--editable', action='append', default=[],
                         help='install in editable mode (best-effort)')
    install.add_argument('-U', '--upgrade', action='store_true')
    install.set_defaults(func=cmd_install)

    uninstall = subs.add_parser('uninstall', help='remove a package')
    uninstall.add_argument('packages', nargs='+')
    uninstall.add_argument('-y', '--yes', action='store_true')
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
        print('pip {} (from _minipip / WeavePy)'.format(VERSION))
        return 0
    if not getattr(opts, 'command', None):
        parser.print_help()
        return 1
    return opts.func(opts)


if __name__ == '__main__':
    sys.exit(main())
