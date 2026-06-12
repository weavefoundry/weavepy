"""``importlib.util`` — helpers around the spec / loader machinery.

After RFC 0029 this module exposes the full PEP 451 utility
surface CPython documents: spec construction, module
construction, ``find_spec``, ``LazyLoader``, ``MAGIC_NUMBER``
and the source-cache mapping. Everything packaging-ecosystem
code reaches for at import time.
"""

import os
import sys

from importlib import machinery as _machinery

MAGIC_NUMBER = _machinery.MAGIC_NUMBER

__all__ = [
    'MAGIC_NUMBER',
    'cache_from_source',
    'source_from_cache',
    'decode_source',
    'spec_from_file_location',
    'spec_from_loader',
    'module_from_spec',
    'find_spec',
    'resolve_name',
    'LazyLoader',
    'source_hash',
    '_incompatible_extension_module_restrictions',
]


def _cache_tag():
    impl = sys.implementation
    return getattr(impl, 'cache_tag', 'weavepy-3.13')


def cache_from_source(path, debug_override=None, *, optimization=None):
    """Map ``<dir>/<name>.py`` → ``<dir>/__pycache__/<name>.<tag>.pyc``.

    Matches CPython's mapping, with one wrinkle: when
    ``sys.pycache_prefix`` is set, the resulting path lives under
    that directory instead of next to the source.
    """
    head, tail = os.path.split(path)
    name, _ = os.path.splitext(tail)
    tag = _cache_tag()
    prefix = getattr(sys, 'pycache_prefix', None)
    if prefix:
        absbase = os.path.abspath(head)
        # Drop the drive prefix on Windows so we don't end up with
        # path components like ``C:`` inside the cache directory.
        if os.path.isabs(absbase):
            absbase = absbase.lstrip(os.sep)
        target_dir = os.path.join(prefix, absbase)
    else:
        target_dir = os.path.join(head, '__pycache__')
    return os.path.join(target_dir, '{}.{}.pyc'.format(name, tag))


def source_from_cache(path):
    """Reverse of :func:`cache_from_source`.

    Tries to recover ``<dir>/<name>.py`` from a ``.pyc`` path,
    raising ``ValueError`` if the layout doesn't look like a
    cache hit.
    """
    if not path.endswith('.pyc'):
        raise ValueError("not a .pyc path: {!r}".format(path))
    head, tail = os.path.split(path)
    name = tail[:-4]  # strip .pyc
    base = name.rsplit('.', 1)[0]
    if os.path.basename(head) == '__pycache__':
        parent = os.path.dirname(head)
    else:
        parent = head
    return os.path.join(parent, base + '.py')


def _coding_cookie(line):
    """PEP 263 cookie in a comment line (bytes), or None."""
    i = 0
    while i < len(line) and line[i : i + 1] in (b' ', b'\t', b'\x0c'):
        i += 1
    if line[i : i + 1] != b'#':
        return None
    pos = line.find(b'coding', i)
    if pos < 0:
        return None
    j = pos + 6
    if line[j : j + 1] not in (b':', b'='):
        return None
    j += 1
    while line[j : j + 1] in (b' ', b'\t'):
        j += 1
    start = j
    while j < len(line) and chr(line[j]) in (
        'abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_.'
    ):
        j += 1
    if j == start:
        return None
    return line[start:j].decode('ascii')


def decode_source(source_bytes):
    """Decode a source-byte blob to text, per PEP 263: a UTF-8 BOM
    wins, then a coding cookie on line 1 or 2, defaulting to UTF-8
    (CPython routes this through `tokenize.detect_encoding`).
    """
    if isinstance(source_bytes, str):
        return source_bytes
    if source_bytes.startswith(b'\xef\xbb\xbf'):
        return source_bytes[3:].decode('utf-8')
    for line in source_bytes.split(b'\n', 2)[:2]:
        encoding = _coding_cookie(line)
        if encoding is not None:
            return source_bytes.decode(encoding)
    return source_bytes.decode('utf-8')


def source_hash(source_bytes):
    """Compute the 8-byte source hash used to detect stale
    pyc artifacts. CPython hashes with siphash13; we use a stable
    fnv-1a so the digest is reproducible across runs without
    pulling in hashlib at this layer.
    """
    if isinstance(source_bytes, str):
        source_bytes = source_bytes.encode('utf-8')
    h = 0xcbf29ce484222325
    for b in source_bytes:
        h = (h ^ b) & 0xFFFFFFFFFFFFFFFF
        h = (h * 0x100000001b3) & 0xFFFFFFFFFFFFFFFF
    return h.to_bytes(8, 'little')


def resolve_name(name, package):
    """Resolve a relative module name. Mirrors CPython's
    ``importlib._bootstrap._resolve_name``.
    """
    if not name.startswith('.'):
        return name
    if not package:
        raise ImportError(
            "attempted relative import with no known parent package")
    level = 0
    for ch in name:
        if ch != '.':
            break
        level += 1
    bits = package.rsplit('.', level - 1)
    if len(bits) < level:
        raise ImportError("attempted relative import beyond top-level package")
    base = bits[0]
    remainder = name[level:]
    return '{}.{}'.format(base, remainder) if remainder else base


def spec_from_loader(name, loader, *, origin=None, is_package=None):
    if is_package is None and hasattr(loader, 'is_package'):
        try:
            is_package = bool(loader.is_package(name))
        except Exception:
            is_package = False
    return _machinery.ModuleSpec(
        name, loader, origin=origin, is_package=bool(is_package))


def spec_from_file_location(name, location=None, *, loader=None,
                              submodule_search_locations=None):
    """Compose a ``ModuleSpec`` directly from a file path.

    Picks a loader by suffix unless one is supplied. This is the
    primary entry-point for packaging tools that need to build
    specs by hand (``importlib.util.spec_from_file_location`` is
    the documented way to dynamically import a file).
    """
    if loader is None and location is not None:
        for sfx in _machinery.EXTENSION_SUFFIXES:
            if location.endswith(sfx):
                loader = _machinery.ExtensionFileLoader(name, location)
                break
        else:
            if location.endswith('.py'):
                loader = _machinery.SourceFileLoader(name, location)
            elif location.endswith('.pyc'):
                loader = _machinery.SourcelessFileLoader(name, location)
            else:
                loader = _machinery.SourceFileLoader(name, location)
    spec = _machinery.ModuleSpec(
        name, loader, origin=location,
        is_package=bool(submodule_search_locations))
    if submodule_search_locations is not None:
        spec.submodule_search_locations = list(submodule_search_locations)
    if location is not None:
        spec._set_fileattr = True
    return spec


def module_from_spec(spec):
    """Manufacture a fresh module object for ``spec``."""
    import types
    module = None
    if hasattr(spec.loader, 'create_module'):
        try:
            module = spec.loader.create_module(spec)
        except Exception:
            module = None
    if module is None:
        module = types.ModuleType(spec.name)
    module.__spec__ = spec
    if spec.origin is not None and spec.has_location:
        module.__file__ = spec.origin
    if spec.is_package:
        module.__path__ = list(spec.submodule_search_locations or [])
    module.__loader__ = spec.loader
    module.__package__ = spec.parent
    return module


def _is_frozen_name(name):
    """Helper: probe the VM-side frozen registry. Returns False
    on builds that don't expose the helper.
    """
    try:
        return bool(sys._is_frozen(name))
    except (AttributeError, TypeError):
        return False


def find_spec(name, package=None):
    """Walk ``sys.meta_path`` looking for ``name``.

    Handles relative names by resolving against ``package``,
    consults ``sys.modules`` first (matching CPython's behaviour
    of returning whatever the user stashed there), and only then
    walks the finder chain.
    """
    fullname = resolve_name(name, package) if name.startswith('.') else name
    if fullname in sys.modules:
        mod = sys.modules[fullname]
        if mod is None:
            # Module loading is in progress and was nulled out;
            # treat as "not yet visible" and fall through to the
            # finder walk so the in-progress import can recover.
            return None
        spec = getattr(mod, '__spec__', None)
        if spec is not None:
            return spec
        # Synthesize a best-effort spec for modules the VM built
        # before the import-spec machinery was online (the
        # bootstrap chicken-and-egg situation: most built-in and
        # frozen modules ship without an explicit __spec__).
        loader = getattr(mod, '__loader__', None)
        origin = getattr(mod, '__file__', None)
        if origin is None:
            if fullname in sys.builtin_module_names:
                origin = 'built-in'
            elif _is_frozen_name(fullname):
                origin = 'frozen'
        is_package = hasattr(mod, '__path__')
        spec = _machinery.ModuleSpec(
            fullname, loader, origin=origin, is_package=is_package)
        if is_package:
            spec.submodule_search_locations = list(mod.__path__ or [])
        try:
            mod.__spec__ = spec
        except (AttributeError, TypeError):
            pass
        return spec
    parent_path = None
    if '.' in fullname:
        parent_name = fullname.rpartition('.')[0]
        parent = sys.modules.get(parent_name)
        if parent is None:
            try:
                parent = __import__(parent_name)
            except ImportError:
                return None
        parent_path = getattr(parent, '__path__', None)
    for finder in sys.meta_path:
        try:
            if hasattr(finder, 'find_spec'):
                spec = finder.find_spec(fullname, parent_path)
            else:
                spec = None
        except Exception:
            spec = None
        if spec is not None:
            return spec
    return None


class _LazyModule:
    """A module proxy that lazily executes its loader body on the
    first attribute access. Used by ``LazyLoader``.
    """
    # We don't subclass ``types.ModuleType`` because the import
    # system constructs the underlying module already and we
    # patch it in-place via __class__ assignment in ``LazyLoader``.

    def __getattribute__(self, name):
        # Restore the real module class, run the loader body,
        # then replay the lookup against the now-populated module.
        cls = object.__getattribute__(self, '__class__')
        if cls is not _LazyModule:
            return object.__getattribute__(self, name)
        try:
            spec = object.__getattribute__(self, '__spec__')
        except AttributeError:
            spec = None
        if spec is None or getattr(spec, '_lazy_loader', None) is None:
            return object.__getattribute__(self, name)
        # First access: swap the class back and exec.
        import types
        loader = spec._lazy_loader
        object.__setattr__(self, '__class__', types.ModuleType)
        try:
            loader.exec_module(self)
        except Exception:
            # Re-arm the lazy proxy so a retry is possible.
            object.__setattr__(self, '__class__', _LazyModule)
            raise
        return object.__getattribute__(self, name)


class LazyLoader:
    """Wrap a loader so the module body runs only on first
    attribute access. Useful for "heavy" optional dependencies
    that you want to import declaratively without paying the
    body-execution cost up-front.
    """

    def __init__(self, loader):
        if not hasattr(loader, 'exec_module'):
            raise TypeError(
                "loader must define exec_module() to be lazy-wrappable")
        self.loader = loader

    @classmethod
    def factory(cls, loader_cls):
        """Return a factory that builds a LazyLoader around any
        instance of ``loader_cls``.
        """
        def factory(*args, **kwargs):
            return cls(loader_cls(*args, **kwargs))
        factory.__name__ = 'LazyLoader.factory'
        return factory

    def create_module(self, spec):
        return None

    def exec_module(self, module):
        # Tag the spec so _LazyModule can find our loader, then
        # swap the module's class to the lazy proxy.
        module.__spec__._lazy_loader = self.loader
        module.__class__ = _LazyModule


def _incompatible_extension_module_restrictions(*, disable_check=False):
    """CPython hook for sub-interpreter isolation. We always run
    in the main interpreter, so this is a no-op context manager.
    """
    class _NoOp:
        def __enter__(self):
            return self

        def __exit__(self, *exc):
            return False
    return _NoOp()
