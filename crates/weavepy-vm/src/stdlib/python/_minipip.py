"""``_minipip`` — a minimal pip-compatible installer.

Bootstraps real pip. Implements just enough of pip's CLI to install
pure-Python wheels from PyPI or a local path:

    pip install <wheel-file>
    pip install <package>
    pip install -r requirements.txt
    pip uninstall <package>
    pip list
    pip show <package>
    pip --version

Build-from-source (PEP 517), extras with markers, and the full
resolver are intentionally out of scope. The point is to bootstrap
real pip; once it's installed everything else falls out.
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


__all__ = ['main']

VERSION = '0.1.0+weavepy'
DEFAULT_INDEX = 'https://pypi.org/simple/'
USER_AGENT = 'weavepy-minipip/{}'.format(VERSION)


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


def _find_wheel_on_index(name, index_url, python_version=None):
    """Look up ``name`` on a PEP 503 simple index, return the URL of
    the best-matching pure-Python wheel.
    """
    if not index_url.endswith('/'):
        index_url += '/'
    project_url = urljoin(index_url, _normalize(name) + '/')
    html = _http_text(project_url)
    candidates = []
    for href, label in _LINK_RE.findall(html):
        if not label.endswith('.whl'):
            continue
        if not _is_compatible_wheel(label):
            continue
        # Strip any fragment.
        url = href.split('#', 1)[0]
        if not url.startswith('http'):
            url = urljoin(project_url, url)
        version = _wheel_version(label)
        candidates.append((version, label, url))
    if not candidates:
        return None, None
    # Sort by (version desc, tag-score desc) so we prefer the latest
    # release, breaking ties in favour of the most-specific wheel
    # we can satisfy.
    candidates.sort(
        key=lambda t: (_version_key(t[0]), _wheel_tag_score(t[1])),
        reverse=True,
    )
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
    for spec in targets:
        try:
            _install_spec(spec, index_url=args.index_url,
                            quiet=args.quiet)
        except Exception as exc:
            print('ERROR: {}: {}'.format(spec, exc), file=sys.stderr)
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


def _install_spec(spec, *, index_url, quiet=False):
    """Install one requirement specifier."""
    if os.path.isfile(spec) and spec.endswith('.whl'):
        if not quiet:
            print('Installing wheel: {}'.format(spec))
        _install_wheel(spec)
        return
    name, _, _ = re.split(r'[<>=!~]', spec, maxsplit=1)
    name = name.strip()
    if not quiet:
        print('Looking up {} on {}'.format(name, index_url))
    label, url = _find_wheel_on_index(name, index_url)
    if url is None:
        raise RuntimeError('no compatible wheel found for {!r}'.format(name))
    if not quiet:
        print('Downloading {}'.format(label))
    blob = _http_get(url)
    with tempfile.NamedTemporaryFile(suffix='.whl', delete=False) as tmp:
        tmp.write(blob)
        tmp_path = tmp.name
    try:
        _install_wheel(tmp_path)
    finally:
        try:
            os.remove(tmp_path)
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


def main(argv=None):
    """``python -m _minipip``."""
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
    install.set_defaults(func=cmd_install)

    uninstall = subs.add_parser('uninstall', help='remove a package')
    uninstall.add_argument('packages', nargs='+')
    uninstall.add_argument('-y', '--yes', action='store_true')
    uninstall.set_defaults(func=cmd_uninstall)

    list_cmd = subs.add_parser('list', help='list installed packages')
    list_cmd.set_defaults(func=cmd_list)

    show = subs.add_parser('show', help='show package metadata')
    show.add_argument('packages', nargs='+')
    show.set_defaults(func=cmd_show)

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
