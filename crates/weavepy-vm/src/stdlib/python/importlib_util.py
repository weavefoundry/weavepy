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
    return getattr(impl, 'cache_tag', 'weavepy-313')


def cache_from_source(path, debug_override=None, *, optimization=None):
    """Map ``<dir>/<name>.py`` → ``<dir>/__pycache__/<name>.<tag>.pyc``.

    Matches CPython's mapping, with one wrinkle: when
    ``sys.pycache_prefix`` is set, the resulting path lives under
    that directory instead of next to the source.
    """
    if debug_override is not None:
        import warnings
        warnings.warn('the debug_override parameter is deprecated; use '
                      "'optimization' instead", DeprecationWarning)
        if optimization is not None:
            raise TypeError(
                'debug_override or optimization must be set to None')
        optimization = '' if debug_override else 1
    path = os.fspath(path)
    head, tail = os.path.split(path)
    name, _ = os.path.splitext(tail)
    tag = _cache_tag()
    if tag is None:
        raise NotImplementedError('sys.implementation.cache_tag is None')
    # PEP 488: `optimization=''` (or None at level 0) is the plain
    # `.pyc`; anything else is embedded as an alphanumeric `.opt-N`
    # segment. A None optimization defers to the interpreter's own
    # level (`-O -m compileall` writes `.opt-1` artifacts —
    # `test_compileall.test_pep3147_paths_optimize`).
    if optimization is None:
        optimization = sys.flags.optimize
        if optimization == 0:
            optimization = ''
    optimization = str(optimization)
    if optimization:
        if not optimization.isalnum():
            raise ValueError(
                '{!r} is not alphanumeric'.format(optimization))
        filename = '{}.{}.opt-{}.pyc'.format(name, tag, optimization)
    else:
        filename = '{}.{}.pyc'.format(name, tag)
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
    return os.path.join(target_dir, filename)


def source_from_cache(path):
    """Reverse of :func:`cache_from_source`.

    Tries to recover ``<dir>/<name>.py`` from a ``.pyc`` path,
    raising ``ValueError`` if the layout doesn't look like a
    cache hit (CPython `_bootstrap_external.source_from_cache`
    validation, verbatim — test_importlib.test_util.PEP3147Tests).
    """
    if _cache_tag() is None:
        raise NotImplementedError('sys.implementation.cache_tag is None')
    path = os.fspath(path)
    head, pycache_filename = os.path.split(path)
    found_in_pycache_prefix = False
    pycache_prefix = getattr(sys, 'pycache_prefix', None)
    if pycache_prefix is not None:
        stripped_path = pycache_prefix.rstrip(os.path.sep)
        if head.startswith(stripped_path + os.path.sep):
            head = head[len(stripped_path):]
            found_in_pycache_prefix = True
    if not found_in_pycache_prefix:
        head, pycache = os.path.split(head)
        if pycache != '__pycache__':
            raise ValueError(
                f'__pycache__ not bottom-level directory in {path!r}')
    dot_count = pycache_filename.count('.')
    if dot_count not in {2, 3}:
        raise ValueError(
            f'expected only 2 or 3 dots in {pycache_filename!r}')
    elif dot_count == 3:
        optimization = pycache_filename.rsplit('.', 2)[-2]
        if not optimization.startswith('opt-'):
            raise ValueError(
                "optimization portion of filename does not start "
                "with {!r}".format('opt-'))
        opt_level = optimization[len('opt-'):]
        if not opt_level.isalnum():
            raise ValueError(
                f"optimization level {optimization!r} is not an "
                "alphanumeric value")
    base_filename = pycache_filename.partition('.')[0]
    return os.path.join(head, base_filename + '.py')


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
    bom_found = source_bytes.startswith(b'\xef\xbb\xbf')
    if bom_found:
        source_bytes = source_bytes[3:]
    for line in source_bytes.split(b'\n', 2)[:2]:
        encoding = _coding_cookie(line)
        if encoding is not None:
            # `tokenize.detect_encoding` semantics: an unknown cookie
            # is a SyntaxError, and a BOM tolerates only a literal
            # `utf-8` cookie (the interpreter rejects `utf8` + BOM —
            # `test_py_compile.test_bad_coding`).
            import codecs
            try:
                codecs.lookup(encoding)
            except LookupError:
                raise SyntaxError('unknown encoding: ' + encoding)
            if bom_found:
                if encoding.replace('_', '-').lower() != 'utf-8':
                    raise SyntaxError('encoding problem: utf-8')
                encoding = 'utf-8'
            return source_bytes.decode(encoding)
        # A cookie on line 2 only counts if line 1 is blank or a comment.
        stripped = line.strip(b' \t\x0c\r')
        if stripped and not stripped.startswith(b'#'):
            break
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
    # CPython: a loader with `get_filename` describes an on-disk (or
    # in-archive) location — route through `spec_from_file_location`
    # so `origin` and package search locations come from the loader
    # (the verbatim `zipimport` relies on this to shape its specs).
    if hasattr(loader, 'get_filename'):
        if is_package is None:
            return spec_from_file_location(name, loader=loader)
        search = [] if is_package else None
        return spec_from_file_location(name, loader=loader,
                                       submodule_search_locations=search)
    if is_package is None and hasattr(loader, 'is_package'):
        try:
            is_package = bool(loader.is_package(name))
        except Exception:
            is_package = False
    return _machinery.ModuleSpec(
        name, loader, origin=origin, is_package=bool(is_package))


# Sentinel: "ask the loader whether this is a package" (CPython's
# `_bootstrap_external._POPULATE`).
_POPULATE = object()


def spec_from_file_location(name, location=None, *, loader=None,
                              submodule_search_locations=_POPULATE):
    """Compose a ``ModuleSpec`` directly from a file path.

    Picks a loader by suffix unless one is supplied. This is the
    primary entry-point for packaging tools that need to build
    specs by hand (``importlib.util.spec_from_file_location`` is
    the documented way to dynamically import a file).
    """
    if location is None:
        # A loader that knows its file (CPython's default handling).
        location = '<unknown>'
        if hasattr(loader, 'get_filename'):
            try:
                location = loader.get_filename(name)
            except ImportError:
                pass
    else:
        location = os.fspath(location)
    if loader is None:
        for sfx in _machinery.EXTENSION_SUFFIXES:
            if location.endswith(sfx):
                loader = _machinery.ExtensionFileLoader(name, location)
                break
        else:
            if location.endswith('.pyc'):
                loader = _machinery.SourcelessFileLoader(name, location)
            else:
                loader = _machinery.SourceFileLoader(name, location)
    spec = _machinery.ModuleSpec(name, loader, origin=location)
    spec._set_fileattr = True
    if submodule_search_locations is _POPULATE:
        if hasattr(loader, 'is_package'):
            try:
                is_package = loader.is_package(name)
            except ImportError:
                pass
            else:
                if is_package:
                    spec.submodule_search_locations = []
    else:
        spec.submodule_search_locations = (
            list(submodule_search_locations)
            if submodule_search_locations is not None else None)
    if spec.submodule_search_locations == []:
        if location:
            dirname = os.path.split(location)[0]
            spec.submodule_search_locations.append(dirname)
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
        # CPython `_init_module_attrs` also stamps `__cached__` for
        # located specs (test_file_loader asserts it after load_module).
        if spec.cached is not None:
            try:
                module.__cached__ = spec.cached
            except AttributeError:
                pass
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


def _find_spec_from_path(name, path=None):
    """Return the spec for the specified module.

    First, sys.modules is checked to see if the module was already imported.
    If so, then sys.modules[name].__spec__ is returned. If that happens to be
    set to None, then ValueError is raised. If the module is not in
    sys.modules, then sys.meta_path is searched for a suitable spec with the
    value of 'path' given to the finders. None is returned if no spec could
    be found.

    Dotted names do not have their parent packages implicitly imported. You
    will most likely need to explicitly import all parent packages in the
    proper order for a submodule to get the correct spec.

    Private CPython surface (`importlib/util.py`), but load-bearing for
    stdlib consumers: `pyclbr._readmodule` resolves every module through it.
    """
    if name not in sys.modules:
        # `_bootstrap._find_spec`: walk sys.meta_path with the raw search
        # path — no parent-package resolution on this entry point.
        for finder in sys.meta_path:
            find_spec_method = getattr(finder, 'find_spec', None)
            if find_spec_method is None:
                continue
            spec = find_spec_method(name, path)
            if spec is not None:
                return spec
        return None
    module = sys.modules[name]
    if module is None:
        return None
    try:
        spec = module.__spec__
    except AttributeError:
        # CPython raises ValueError('...__spec__ is not set'): its modules
        # always carry the attribute. WeavePy builds some modules before
        # the spec machinery is online, so repair with the same
        # best-effort spec synthesis `find_spec` applies.
        return find_spec(name)
    if spec is None:
        raise ValueError(f'{name}.__spec__ is None')
    return spec


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
        if origin is not None and origin not in ('built-in', 'frozen'):
            # A real `__file__` origin is a location (CPython
            # `_spec_from_module` keeps `has_location` in sync).
            spec._set_fileattr = True
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
            # Propagates ImportError, exactly like CPython's
            # `__import__(parent_name, fromlist=['__path__'])`.
            __import__(parent_name)
            parent = sys.modules[parent_name]
        try:
            parent_path = parent.__path__
        except AttributeError as e:
            # CPython raises here (not "return None"): asking for a
            # submodule of a non-package is a ModuleNotFoundError —
            # runpy turns it into `python -m builtins.x`'s
            # "Error while finding module specification" message.
            raise ModuleNotFoundError(
                f"__path__ attribute not found on {parent_name!r} "
                f"while trying to find {fullname!r}", name=fullname) from e
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
