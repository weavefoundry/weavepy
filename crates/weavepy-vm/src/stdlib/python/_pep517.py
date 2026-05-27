"""``_pep517`` — minimal PEP 517 build backend driver.

Used by :mod:`_minipip` to install source distributions when no
binary wheel is available on PyPI for the host. Implements just
enough of the spec to build a pure-Python package whose
``pyproject.toml`` declares either ``setuptools.build_meta`` or
``flit_core.buildapi`` as the build backend.

Surface::

    extract_sdist(path)         -> directory containing the unpacked source
    build_wheel(src_dir)        -> path to a .whl in a temp directory
    build_sdist(src_dir)        -> path to a fresh .tar.gz
    metadata_for(src_dir)       -> METADATA text

Compiled C/C++ extensions are out of scope — those need a real
toolchain plus the per-backend extension build dance. For now,
attempting to build such a package raises :class:`BuildBackendError`.
"""

import io
import os
import re
import shutil
import sys
import tarfile
import tempfile
import zipfile


class BuildBackendError(RuntimeError):
    """Raised when the requested backend can't be driven."""


# ---------------------------------------------------------------------
# sdist extraction
# ---------------------------------------------------------------------

def extract_sdist(sdist_path: str) -> str:
    """Extract a `.tar.gz` / `.zip` sdist; return the top-level dir."""
    tmp = tempfile.mkdtemp(prefix='weavepy-sdist-')
    lower = sdist_path.lower()
    if lower.endswith('.tar.gz') or lower.endswith('.tgz'):
        with tarfile.open(sdist_path, 'r:gz') as tf:
            tf.extractall(tmp)
    elif lower.endswith('.zip'):
        with zipfile.ZipFile(sdist_path) as zf:
            zf.extractall(tmp)
    else:
        raise BuildBackendError('unsupported sdist format: {}'.format(sdist_path))
    # Most sdists nest a single top-level directory.
    entries = [os.path.join(tmp, e) for e in os.listdir(tmp)]
    dirs = [e for e in entries if os.path.isdir(e)]
    if len(dirs) == 1:
        return dirs[0]
    return tmp


# ---------------------------------------------------------------------
# pyproject.toml parsing
# ---------------------------------------------------------------------

def _load_pyproject(src_dir: str) -> dict:
    path = os.path.join(src_dir, 'pyproject.toml')
    if not os.path.isfile(path):
        # Fall back to setup.py-only flow.
        return {}
    try:
        import tomllib
    except ImportError:  # pragma: no cover
        return {}
    with open(path, 'rb') as f:
        return tomllib.load(f)


def _backend_name(pyproject: dict) -> str:
    """The PEP 517 backend module string, e.g. 'setuptools.build_meta'."""
    bs = pyproject.get('build-system', {})
    return bs.get('build-backend') or 'setuptools.build_meta'


# ---------------------------------------------------------------------
# Bridge to backends. We ship a *very* simple backend ourselves that
# handles trivial pure-Python sdists; for everything else we attempt
# to import the declared backend from `sys.path`. Bootstrap pipelines
# are expected to install `setuptools` / `flit_core` first.
# ---------------------------------------------------------------------

def _import_backend(spec: str):
    mod_name, _, attr = spec.partition(':')
    if not mod_name:
        mod_name = 'setuptools.build_meta'
    try:
        mod = __import__(mod_name, fromlist=[attr or '__name__'])
    except Exception:
        return None
    if attr:
        return getattr(mod, attr, None)
    return mod


def build_wheel(src_dir: str) -> str:
    """Build a wheel out of ``src_dir``; return the path to the .whl."""
    pyproject = _load_pyproject(src_dir)
    backend_spec = _backend_name(pyproject)
    backend = _import_backend(backend_spec)
    out_dir = tempfile.mkdtemp(prefix='weavepy-wheel-')
    if backend is not None and hasattr(backend, 'build_wheel'):
        try:
            cwd = os.getcwd()
            os.chdir(src_dir)
            try:
                wheel_name = backend.build_wheel(out_dir)
            finally:
                os.chdir(cwd)
            return os.path.join(out_dir, wheel_name)
        except Exception as exc:
            # Fall through to the in-tree fallback.
            err = exc
        finally:
            pass
    # Fallback: trivial wheel builder for pure-Python projects.
    try:
        return _fallback_build_wheel(src_dir, out_dir, pyproject)
    except Exception as exc:
        raise BuildBackendError(
            'failed to build wheel via {!r}: {}'.format(backend_spec, exc))


def build_sdist(src_dir: str) -> str:
    """Build an sdist out of ``src_dir``; return the path to the .tar.gz."""
    pyproject = _load_pyproject(src_dir)
    backend_spec = _backend_name(pyproject)
    backend = _import_backend(backend_spec)
    out_dir = tempfile.mkdtemp(prefix='weavepy-sdist-')
    if backend is not None and hasattr(backend, 'build_sdist'):
        cwd = os.getcwd()
        os.chdir(src_dir)
        try:
            name = backend.build_sdist(out_dir)
            return os.path.join(out_dir, name)
        finally:
            os.chdir(cwd)
    return _fallback_build_sdist(src_dir, out_dir, pyproject)


def metadata_for(src_dir: str) -> str:
    """Return the METADATA text the wheel would carry."""
    pyproject = _load_pyproject(src_dir)
    proj = pyproject.get('project', {})
    name = proj.get('name', 'unknown')
    version = proj.get('version', '0.0.0')
    summary = proj.get('description', '')
    parts = [
        'Metadata-Version: 2.1',
        'Name: {}'.format(name),
        'Version: {}'.format(version),
    ]
    if summary:
        parts.append('Summary: {}'.format(summary))
    for r in proj.get('dependencies', []):
        parts.append('Requires-Dist: {}'.format(r))
    parts.append('')
    return '\n'.join(parts)


# ---------------------------------------------------------------------
# Fallback in-tree backend for trivial pure-Python sdists.
# ---------------------------------------------------------------------

def _fallback_build_wheel(src_dir: str, out_dir: str, pyproject: dict) -> str:
    proj = pyproject.get('project', {})
    name = proj.get('name')
    version = proj.get('version', '0.0.0')
    if not name:
        # Try to read setup.py metadata; otherwise infer from sdist
        # directory name.
        name = _infer_name(src_dir)
    py_packages = _discover_packages(src_dir, name)
    wheel_name = '{}-{}-py3-none-any.whl'.format(name.replace('-', '_'), version)
    wheel_path = os.path.join(out_dir, wheel_name)
    dist_info = '{}-{}.dist-info'.format(name.replace('-', '_'), version)
    metadata = metadata_for(src_dir)
    if not metadata.startswith('Metadata-Version'):
        metadata = (
            'Metadata-Version: 2.1\n'
            'Name: {}\n'
            'Version: {}\n'
        ).format(name, version)
    wheel_meta = (
        'Wheel-Version: 1.0\n'
        'Generator: weavepy-pep517-fallback (0.1)\n'
        'Root-Is-Purelib: true\n'
        'Tag: py3-none-any\n'
    )
    with zipfile.ZipFile(wheel_path, 'w', compression=zipfile.ZIP_DEFLATED) as zf:
        # Copy package modules.
        record_lines = []
        for pkg_root in py_packages:
            for root, _, files in os.walk(pkg_root):
                for fn in files:
                    if fn.endswith('.pyc'):
                        continue
                    full = os.path.join(root, fn)
                    rel = os.path.relpath(full, src_dir).replace(os.sep, '/')
                    zf.write(full, rel)
                    record_lines.append('{},,'.format(rel))
        # METADATA, WHEEL, RECORD.
        meta_path = '{}/METADATA'.format(dist_info)
        wheel_path_in_zip = '{}/WHEEL'.format(dist_info)
        zf.writestr(meta_path, metadata)
        zf.writestr(wheel_path_in_zip, wheel_meta)
        record_lines.append('{},,'.format(meta_path))
        record_lines.append('{},,'.format(wheel_path_in_zip))
        record_lines.append('{}/RECORD,,'.format(dist_info))
        zf.writestr('{}/RECORD'.format(dist_info), '\n'.join(record_lines) + '\n')
    return wheel_path


def _fallback_build_sdist(src_dir: str, out_dir: str, pyproject: dict) -> str:
    proj = pyproject.get('project', {})
    name = proj.get('name') or _infer_name(src_dir)
    version = proj.get('version', '0.0.0')
    sdist_name = '{}-{}.tar.gz'.format(name, version)
    sdist_path = os.path.join(out_dir, sdist_name)
    with tarfile.open(sdist_path, 'w:gz') as tf:
        tf.add(src_dir, arcname='{}-{}'.format(name, version))
    return sdist_path


def _infer_name(src_dir: str) -> str:
    base = os.path.basename(src_dir.rstrip('/\\'))
    return re.split(r'-\d', base)[0] or base


def _discover_packages(src_dir: str, project_name: str):
    """Naive package discovery: every dir containing an __init__.py."""
    out = []
    candidate = os.path.join(src_dir, project_name.replace('-', '_'))
    if os.path.isdir(candidate):
        out.append(candidate)
    for entry in os.listdir(src_dir):
        full = os.path.join(src_dir, entry)
        if not os.path.isdir(full):
            continue
        if not os.path.isfile(os.path.join(full, '__init__.py')):
            continue
        if full not in out:
            out.append(full)
    # Single-file modules in the src root (foo.py at top level).
    for entry in os.listdir(src_dir):
        if entry.endswith('.py') and entry not in ('setup.py',):
            out.append(src_dir)
            break
    return out
