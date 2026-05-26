"""RFC 0029 import-machinery surface tests.

These tests don't actually load a C extension (a real `.so` may not be
available in the regrtest environment) — instead they exercise:

* `importlib.machinery.ExtensionFileLoader` exists and has the
  expected shape;
* `_imp._load_dynamic` is callable;
* the path-hook chain is wired so `FileFinder` will look for
  extension suffixes alongside `.py` files;
* `_minipip`'s wheel-tag heuristics accept the platform / ABI / Python
  tags of the running interpreter (so a real numpy wheel would in
  principle be installable);
* a synthetic binary wheel can be unpacked into a private
  site-packages without errors.
"""

import os
import sys
import tempfile
import zipfile

# ---------------------------------------------------------------------
# importlib.machinery
# ---------------------------------------------------------------------

import importlib.machinery as machinery

assert hasattr(machinery, 'ExtensionFileLoader')
assert hasattr(machinery, 'FileFinder')
assert hasattr(machinery, 'PathFinder')
assert hasattr(machinery, 'EXTENSION_SUFFIXES')
exts = machinery.EXTENSION_SUFFIXES
assert isinstance(exts, list) and exts, exts
# Whatever the host advertises, at least one of these should be in it.
candidates = {'.so', '.dylib', '.pyd', '.abi3.so'}
assert any(s in candidates or s.endswith('.so') for s in exts), exts

loader = machinery.ExtensionFileLoader('demo', '/tmp/_demo.so')
assert loader.name == 'demo'
assert loader.path == '/tmp/_demo.so'
assert loader.get_source() is None
assert loader.get_code() is None
assert loader.is_package() is False

# ---------------------------------------------------------------------
# _imp surface
# ---------------------------------------------------------------------

import _imp

assert callable(_imp._load_dynamic)
assert callable(_imp.create_dynamic)
assert callable(_imp.exec_dynamic)
assert callable(_imp.is_builtin)
assert callable(_imp.is_frozen)

# ---------------------------------------------------------------------
# sys.meta_path / sys.path_hooks contain PathFinder + FileFinder hooks.
# ---------------------------------------------------------------------

assert any(getattr(f, '__name__', '') in ('PathFinder', 'BuiltinImporter',
                                              'FrozenImporter')
              or type(f).__name__ in ('PathFinder', 'BuiltinImporter',
                                      'FrozenImporter')
              for f in sys.meta_path), sys.meta_path
assert sys.path_hooks, sys.path_hooks

# ---------------------------------------------------------------------
# Wheel-tag matcher: accept the current interpreter's tag triple.
# ---------------------------------------------------------------------

import _minipip
maj, minr = sys.version_info[:2]
py_tag = 'cp%d%d' % (maj, minr)
abi_tag = 'cp%d%d' % (maj, minr)
plat_tag = 'any'

assert _minipip._is_compatible_wheel('pkg-1.0-cp%d%d-cp%d%d-any.whl' % (maj, minr, maj, minr))
assert _minipip._is_compatible_wheel('pkg-1.0-py3-none-any.whl')
assert _minipip._is_compatible_wheel('pkg-1.0-py%d-none-any.whl' % maj)
assert _minipip._is_compatible_wheel('pkg-1.0-py%d.py3-none-any.whl' % maj)

# Multi-tag wheels: `py2.py3-none-any` and dotted ABI / platform tags
# must all parse cleanly.
assert _minipip._is_compatible_wheel('pkg-1.0-py2.py3-none-any.whl')

# Wheels for a Python we can't run must be rejected.
assert not _minipip._is_compatible_wheel('pkg-1.0-cp99-cp99-any.whl')
assert not _minipip._is_compatible_wheel('pkg-1.0-py99-none-any.whl')

# ---------------------------------------------------------------------
# Wheel installation round-trip with a synthetic .so payload (the
# .so isn't actually loadable — we only verify the unpack honours
# extension-suffix files and `.data/` routing).
# ---------------------------------------------------------------------

with tempfile.TemporaryDirectory() as tmp:
    wheel_path = os.path.join(tmp, 'demo-1.0-py3-none-any.whl')
    with zipfile.ZipFile(wheel_path, 'w') as zf:
        # Pure-Python payload.
        zf.writestr('demo/__init__.py', 'VERSION = "1.0"\n')
        zf.writestr('demo/_native.so', b'\x7fELF...not really')
        zf.writestr('demo-1.0.dist-info/METADATA',
                    'Metadata-Version: 2.1\nName: demo\nVersion: 1.0\n')
        zf.writestr('demo-1.0.dist-info/WHEEL',
                    'Wheel-Version: 1.0\nGenerator: weavepy-regrtest\n')
        zf.writestr('demo-1.0.dist-info/RECORD', '')

    site_packages = os.path.join(tmp, 'site-packages')
    installed = _minipip._install_wheel(wheel_path, dest=site_packages)
    assert installed, 'expected installed files'

    # Verify both pure-Python and extension files landed.
    py_paths = [p for p in installed if p.endswith('__init__.py')]
    so_paths = [p for p in installed if p.endswith('_native.so')]
    assert py_paths, installed
    assert so_paths, installed

    # The `.so` payload should be marked executable on POSIX.
    if os.name != 'nt':
        mode = os.stat(so_paths[0]).st_mode
        assert mode & 0o111, mode  # at least one execute bit

    # Imports should work for the pure-Python portion if we add the
    # newly-installed location to sys.path.
    sys.path.insert(0, site_packages)
    try:
        import demo
        assert demo.VERSION == '1.0', demo.VERSION
    finally:
        sys.path.remove(site_packages)
        sys.modules.pop('demo', None)

print('extension-import RFC 0029 surface OK')
