"""``importlib.machinery`` — finders, loaders, and module specs.

This is the "low-level" import API. ``pip``, ``setuptools``,
``pluggy``, and most of the packaging ecosystem reach into this
module to construct ``ModuleSpec`` objects, instantiate
``SourceFileLoader`` for plain ``.py`` files, or register
``MetaPathFinder`` subclasses on ``sys.meta_path``.

After RFC 0029 this is a faithful PEP 451 implementation: every
piece of public surface CPython documents in
``importlib.machinery`` is present, with semantics close enough
to what real numpy / pluggy / pytest / setuptools see at runtime
that introspection round-trips correctly.
"""

import os
import sys
import marshal

SOURCE_SUFFIXES = ['.py']
BYTECODE_SUFFIXES = ['.pyc']
# RFC 0029: track the full list of extension suffixes CPython
# recognises on each platform, including the ABI-tagged variants
# that real wheels publish (`*.cpython-313-darwin.so` etc.). Order
# matters: the first match wins, and the implementation-specific
# tags take precedence over the bare suffixes so a custom-built
# extension for *this* runtime is preferred over a generic one
# that happens to be in the same directory.
if sys.platform == 'darwin':
    EXTENSION_SUFFIXES = [
        '.cpython-313-darwin.so',
        '.abi3.so',
        '.so',
        '.dylib',
    ]
elif sys.platform.startswith('linux'):
    EXTENSION_SUFFIXES = [
        '.cpython-313-x86_64-linux-gnu.so',
        '.cpython-313-aarch64-linux-gnu.so',
        '.abi3.so',
        '.so',
    ]
elif sys.platform == 'win32':
    EXTENSION_SUFFIXES = ['.cp313-win_amd64.pyd', '.pyd', '.dll']
else:
    EXTENSION_SUFFIXES = ['.so']

DEBUG_BYTECODE_SUFFIXES = BYTECODE_SUFFIXES
OPTIMIZED_BYTECODE_SUFFIXES = BYTECODE_SUFFIXES

# Magic bytes used by the WeavePy ``__pycache__`` writer. Kept in
# sync with ``crates/weavepy-vm/src/pycache.rs``.
MAGIC_NUMBER = b'WPY0'


def all_suffixes():
    """Every suffix the import system recognises in priority
    order: source, bytecode, then extensions. Matches the shape
    CPython advertises.
    """
    return SOURCE_SUFFIXES + BYTECODE_SUFFIXES + EXTENSION_SUFFIXES


# ---------------------------------------------------------------------
# PEP 451 — ModuleSpec.
# ---------------------------------------------------------------------


class ModuleSpec:
    """PEP 451 module spec.

    A ``ModuleSpec`` is the metadata bundle the import system uses
    to load and re-introspect a module. After construction it
    carries:

    - ``name`` — fully-qualified module name (``'numpy.core.umath'``).
    - ``loader`` — the loader object whose ``exec_module`` will
      run the module's body.
    - ``origin`` — the source file path, ``'built-in'``,
      ``'frozen'``, or ``None`` for synthetic specs.
    - ``submodule_search_locations`` — list of directories for
      packages, ``None`` for plain modules.
    - ``loader_state`` — opaque payload some loaders attach.
    - ``cached`` — path to the ``__pycache__`` artifact, if any.
    - ``parent`` — the package this module lives in (derived from
      ``name``).
    - ``has_location`` — True if ``origin`` is a real filesystem
      path that ``open()`` would accept.
    """

    __slots__ = ('name', 'loader', 'origin', 'submodule_search_locations',
                 'loader_state', 'cached', '_set_fileattr', '_initializing')

    def __init__(self, name, loader, *, origin=None, loader_state=None,
                 is_package=None):
        self.name = name
        self.loader = loader
        self.origin = origin
        self.loader_state = loader_state
        self.submodule_search_locations = [] if is_package else None
        self.cached = None
        self._set_fileattr = origin is not None
        self._initializing = False

    @property
    def parent(self):
        """The package this module belongs to. For a top-level
        module like ``'os'`` this is ``''``; for ``'os.path'`` it
        is ``'os'``; for ``'numpy.core.umath'`` it is
        ``'numpy.core'``.
        """
        if self.submodule_search_locations is None:
            return self.name.rpartition('.')[0]
        return self.name

    @property
    def has_location(self):
        return self._set_fileattr

    @has_location.setter
    def has_location(self, value):
        self._set_fileattr = bool(value)

    @property
    def is_package(self):
        return self.submodule_search_locations is not None

    def __repr__(self):
        parts = ['name={!r}'.format(self.name)]
        if self.loader is not None:
            parts.append('loader={!r}'.format(self.loader))
        if self.origin is not None:
            parts.append('origin={!r}'.format(self.origin))
        if self.submodule_search_locations is not None:
            parts.append('submodule_search_locations={!r}'.format(
                self.submodule_search_locations))
        return 'ModuleSpec({})'.format(', '.join(parts))

    def __eq__(self, other):
        if not isinstance(other, ModuleSpec):
            return NotImplemented
        return (self.name == other.name
                and self.loader == other.loader
                and self.origin == other.origin
                and self.submodule_search_locations
                    == other.submodule_search_locations
                and self.cached == other.cached
                and self.has_location == other.has_location)

    def __hash__(self):
        return hash((self.name, self.origin))


# ---------------------------------------------------------------------
# Loader base + concrete loaders.
# ---------------------------------------------------------------------


class _LoaderBase:
    """Common machinery for the concrete loaders below.

    Subclasses override ``get_source``, ``exec_module``, and
    ``is_package`` as appropriate. The shared parts are
    constructor + equality + filename access, all of which the
    introspection / packaging tools poke at.
    """

    def __init__(self, fullname, path):
        self.name = fullname
        self.path = path

    def __eq__(self, other):
        return (type(self) is type(other) and self.name == other.name
                and self.path == other.path)

    def __hash__(self):
        return hash((type(self).__name__, self.name, self.path))

    def __repr__(self):
        return '{}({!r}, {!r})'.format(type(self).__name__, self.name,
                                          self.path)

    def get_filename(self, fullname=None):
        return self.path

    def is_package(self, fullname=None):
        if not self.path:
            return False
        base = os.path.basename(self.path)
        return base.startswith('__init__.')

    def get_source(self, fullname=None):
        if not self.path:
            return None
        try:
            with open(self.path, 'rb') as f:
                return f.read().decode('utf-8')
        except OSError:
            return None

    def get_code(self, fullname=None):
        source = self.get_source(fullname)
        if source is None:
            return None
        return compile(source, self.path or '<frozen>', 'exec')

    def get_data(self, path):
        with open(path, 'rb') as f:
            return f.read()

    def create_module(self, spec):
        return None

    def exec_module(self, module):
        source = self.get_source()
        if source is None:
            raise ImportError("no source for {!r}".format(self.name),
                              name=self.name, path=self.path)
        code = compile(source, self.path or '<frozen>', 'exec')
        exec(code, module.__dict__)


class SourceFileLoader(_LoaderBase):
    """Load a module from a ``.py`` file on disk.

    This is the workhorse for everything that lives in
    ``sys.path``: every ``import pandas`` reaches this loader,
    which reads the source, compiles it, and executes it in the
    module's globals dict.
    """

    def source_to_code(self, data, path, *, _optimize=-1):
        """Compile a chunk of source bytes."""
        if isinstance(data, (bytes, bytearray)):
            data = data.decode('utf-8')
        return compile(data, path, 'exec')

    def path_stats(self, path):
        st = os.stat(path)
        return {'mtime': st.st_mtime, 'size': st.st_size}

    def set_data(self, path, data, *, _mode=0o666):
        # Write `.pyc` artifacts. RFC 0029 mirrors CPython's
        # API but defers the actual bytecode caching to the
        # VM-side pycache writer.
        parent = os.path.dirname(path)
        if parent and not os.path.isdir(parent):
            try:
                os.makedirs(parent)
            except OSError:
                pass
        try:
            with open(path, 'wb') as f:
                f.write(data)
        except OSError:
            pass


class SourcelessFileLoader(_LoaderBase):
    """Load a module from a ``.pyc`` file (no source available).

    Used by tooling that ships compiled-only distributions. We
    read the WeavePy ``__pycache__`` header and unmarshal the
    embedded code object.
    """

    def get_source(self, fullname=None):
        return None

    def exec_module(self, module):
        try:
            with open(self.path, 'rb') as f:
                data = f.read()
        except OSError as exc:
            raise ImportError("cannot read {!r}: {}".format(self.path, exc),
                              name=self.name, path=self.path)
        if len(data) < 16 or data[:4] != MAGIC_NUMBER:
            raise ImportError("bad magic in {!r}".format(self.path),
                              name=self.name, path=self.path)
        try:
            code = marshal.loads(data[16:])
        except Exception as exc:
            raise ImportError("bad marshal in {!r}: {}".format(self.path, exc),
                              name=self.name, path=self.path)
        exec(code, module.__dict__)


class ExtensionFileLoader(_LoaderBase):
    """Load a CPython-compatible extension module.

    The actual dlopen/PyInit_<name> dance happens inside the VM
    (the C-API loader registered through
    ``weavepy_vm.ext_loader``). This class is the Python-visible
    half: it carries the metadata that introspection / packaging
    tools poke at, and its ``exec_module`` hands the path off to
    the VM hook.
    """

    def get_source(self, fullname=None):
        return None

    def get_code(self, fullname=None):
        return None

    def is_package(self, fullname=None):
        # Extensions are leaves; CPython matches this.
        return False

    def exec_module(self, module):
        # Hand off to the VM's C-API loader. The hook is
        # installed at interpreter start by
        # `weavepy-cli`/`weavepy-vm` glue and looked up here by
        # name via the private bridge module.
        try:
            from _imp import _load_dynamic
        except ImportError:
            raise ImportError(
                "weavepy: extension loader not installed",
                name=self.name, path=self.path)
        loaded = _load_dynamic(self.name, self.path)
        # `_load_dynamic` registers the resulting module in
        # `sys.modules` and returns it. Copy its dict into the
        # spec-allocated module so the caller's reference stays
        # canonical.
        if loaded is not None and loaded is not module:
            module.__dict__.update(loaded.__dict__)


# ---------------------------------------------------------------------
# Finders.
# ---------------------------------------------------------------------


class FileFinder:
    """Walk a single directory looking for any of ``loaders``.

    Constructed via ``FileFinder.path_hook(*loader_details)`` and
    installed on ``sys.path_hooks``; cached in
    ``sys.path_importer_cache`` per directory.
    """

    def __init__(self, path, *loader_details):
        self.path = path
        # Each entry is (loader_cls, [suffixes]).
        self._loaders = list(loader_details)
        self._path_mtime = -1
        self._path_cache = set()
        self._relaxed_path_cache = set()

    @classmethod
    def path_hook(cls, *loader_details):
        """Return a callable that builds a ``FileFinder`` for any
        directory it's handed, raising ``ImportError`` for non-
        directories (signalling to ``PathFinder`` to try the next
        hook).
        """
        def hook(path):
            if not os.path.isdir(path):
                raise ImportError(
                    "only directories are supported", path=path)
            return cls(path, *loader_details)
        hook.__name__ = '_path_hook'
        return hook

    def invalidate_caches(self):
        self._path_mtime = -1
        self._path_cache.clear()
        self._relaxed_path_cache.clear()

    def _fill_cache(self):
        try:
            entries = os.listdir(self.path or '.')
        except OSError:
            entries = []
        self._path_cache = set(entries)
        # The "relaxed" cache lower-cases filenames for case-
        # insensitive filesystems. We always include both flavours
        # so the case-insensitive path still works.
        self._relaxed_path_cache = {e.lower() for e in entries}

    def find_spec(self, fullname, target=None):
        tail = fullname.rpartition('.')[2]
        self._fill_cache()
        # Package: <dir>/<tail>/__init__<suffix>
        pkg_dir = os.path.join(self.path, tail) if self.path else tail
        if os.path.isdir(pkg_dir):
            for loader_cls, suffixes in self._loaders:
                for sfx in suffixes:
                    init = os.path.join(pkg_dir, '__init__' + sfx)
                    if os.path.isfile(init):
                        loader = loader_cls(fullname, init)
                        spec = ModuleSpec(
                            fullname, loader,
                            origin=init, is_package=True)
                        spec.submodule_search_locations = [pkg_dir]
                        return spec
            # PEP 420: directory exists but has no __init__ — that's a
            # namespace package.
            spec = ModuleSpec(fullname, None, origin=None, is_package=True)
            spec.submodule_search_locations = [pkg_dir]
            spec._set_fileattr = False
            return spec
        # Plain module: <dir>/<tail><suffix>
        for loader_cls, suffixes in self._loaders:
            for sfx in suffixes:
                cand = tail + sfx
                if cand in self._path_cache:
                    p = (os.path.join(self.path, cand)
                          if self.path else cand)
                    loader = loader_cls(fullname, p)
                    return ModuleSpec(fullname, loader, origin=p)
        return None


class PathFinder:
    """Walk ``sys.path`` looking for ``fullname``.

    Honours ``sys.path_hooks`` and ``sys.path_importer_cache`` so
    the importer-resolution work is amortised across imports.
    """

    @classmethod
    def invalidate_caches(cls):
        for finder in sys.path_importer_cache.values():
            if hasattr(finder, 'invalidate_caches'):
                try:
                    finder.invalidate_caches()
                except Exception:
                    pass

    @classmethod
    def _path_importer_cache(cls, path):
        """Find or build the finder for one ``sys.path`` entry,
        caching the result in ``sys.path_importer_cache``.
        """
        if path == '':
            path = '.'
        cache = sys.path_importer_cache
        if path in cache:
            return cache[path]
        for hook in sys.path_hooks:
            try:
                finder = hook(path)
            except ImportError:
                continue
            cache[path] = finder
            return finder
        cache[path] = None
        return None

    @classmethod
    def _get_spec(cls, fullname, path, target=None):
        for entry in path:
            if not isinstance(entry, str):
                continue
            finder = cls._path_importer_cache(entry)
            if finder is None:
                continue
            if hasattr(finder, 'find_spec'):
                try:
                    spec = finder.find_spec(fullname, target)
                except (OSError, ImportError):
                    spec = None
                if spec is not None:
                    return spec
        return None

    @classmethod
    def find_spec(cls, fullname, path=None, target=None):
        if path is None:
            path = sys.path
        # Handle namespace packages: collect every contributing
        # directory across `path` before returning.
        namespace_path = []
        spec = cls._get_spec(fullname, path, target)
        if spec is not None:
            if spec.loader is None and spec.submodule_search_locations:
                # Namespace from the first match: keep walking and
                # merge.
                namespace_path.extend(spec.submodule_search_locations)
                for entry in path[path.index(
                        spec.submodule_search_locations[0])
                        if spec.submodule_search_locations[0] in path
                        else len(path):]:
                    finder = cls._path_importer_cache(entry)
                    if finder is None:
                        continue
                    extra = finder.find_spec(fullname, target)
                    if extra is None:
                        continue
                    if extra.loader is not None:
                        # Real loader wins.
                        return extra
                    namespace_path.extend(
                        extra.submodule_search_locations or [])
                if namespace_path:
                    spec.submodule_search_locations = namespace_path
            return spec
        return None


class BuiltinImporter:
    """Spec lookup for modules registered in
    ``sys.builtin_module_names``.

    Always returns specs with ``origin='built-in'`` and a class-
    level loader; this matches CPython's shape and lets
    introspection (``inspect.getfile``, ``importlib.util.find_spec``)
    distinguish built-ins from frozen / file modules.
    """

    @classmethod
    def find_spec(cls, fullname, path=None, target=None):
        if path is not None:
            # Built-ins are always top-level.
            return None
        if fullname in sys.builtin_module_names:
            return ModuleSpec(fullname, cls, origin='built-in')
        return None

    @classmethod
    def find_module(cls, fullname, path=None):
        # Pre-PEP 451 compat shim still called by some libs.
        spec = cls.find_spec(fullname, path)
        return spec.loader if spec is not None else None

    @classmethod
    def create_module(cls, spec):
        if spec.name in sys.modules:
            return sys.modules[spec.name]
        return None

    @classmethod
    def exec_module(cls, module):
        # The actual loading happens in the host VM; if the
        # module is already in sys.modules we have nothing left
        # to do here.
        pass

    @classmethod
    def get_code(cls, fullname):
        return None

    @classmethod
    def get_source(cls, fullname):
        return None

    @classmethod
    def is_package(cls, fullname):
        return False


class FrozenImporter:
    """Spec lookup for the frozen-Python stdlib bundle baked into
    the WeavePy binary.
    """

    @classmethod
    def find_spec(cls, fullname, path=None, target=None):
        if not _is_frozen(fullname):
            return None
        return ModuleSpec(
            fullname, cls, origin='frozen',
            is_package=_is_frozen_package(fullname))

    @classmethod
    def find_module(cls, fullname, path=None):
        spec = cls.find_spec(fullname, path)
        return spec.loader if spec is not None else None

    @classmethod
    def create_module(cls, spec):
        if spec.name in sys.modules:
            return sys.modules[spec.name]
        return None

    @classmethod
    def exec_module(cls, module):
        # Frozen modules are executed by the VM's loader; by the
        # time we reach this hook the module is already
        # populated.
        pass

    @classmethod
    def get_code(cls, fullname):
        return None

    @classmethod
    def get_source(cls, fullname):
        src = sys._get_frozen_source(fullname) if hasattr(
            sys, '_get_frozen_source') else None
        return src

    @classmethod
    def is_package(cls, fullname):
        return _is_frozen_package(fullname)


def _is_frozen(name):
    """Probe the VM-side frozen registry. Falls back to
    ``False`` on older builds that don't expose the helper.
    """
    try:
        return bool(sys._is_frozen(name))
    except (AttributeError, TypeError):
        return False


def _is_frozen_package(name):
    """Heuristic — a frozen module is a package if its source
    looks package-y. CPython has a richer signal; ours is close
    enough.
    """
    if not _is_frozen(name):
        return False
    # Names with a dot are necessarily inside a package; treat
    # the top-level names that ship with us as packages if their
    # frozen source mentions ``__path__`` (the conventional
    # marker).
    src = None
    try:
        src = sys._get_frozen_source(name)
    except (AttributeError, TypeError):
        pass
    if src is None:
        return False
    return '__path__' in src


# Default details installed by the bootstrap so users don't have
# to assemble loader-detail tuples manually.
_LOADER_DETAILS = [
    (ExtensionFileLoader, EXTENSION_SUFFIXES),
    (SourceFileLoader, SOURCE_SUFFIXES),
    (SourcelessFileLoader, BYTECODE_SUFFIXES),
]


def _bootstrap_meta_path():
    """Install the default ``sys.meta_path`` / ``sys.path_hooks``
    if they're empty. Idempotent: re-importing this module won't
    duplicate entries.
    """
    if not getattr(sys, 'meta_path', None):
        sys.meta_path = [BuiltinImporter, FrozenImporter, PathFinder]
    else:
        # Ensure the defaults are present even if user code already
        # populated meta_path.
        for cls in (BuiltinImporter, FrozenImporter, PathFinder):
            if cls not in sys.meta_path:
                sys.meta_path.append(cls)
    if not getattr(sys, 'path_hooks', None):
        sys.path_hooks = [FileFinder.path_hook(*_LOADER_DETAILS)]
    if not isinstance(getattr(sys, 'path_importer_cache', None), dict):
        sys.path_importer_cache = {}


# Run the bootstrap eagerly on first import so the very first
# ``importlib.util.find_spec(...)`` call sees a populated
# meta_path.
_bootstrap_meta_path()


__all__ = [
    'ModuleSpec',
    'SourceFileLoader',
    'SourcelessFileLoader',
    'ExtensionFileLoader',
    'FileFinder',
    'PathFinder',
    'BuiltinImporter',
    'FrozenImporter',
    'SOURCE_SUFFIXES',
    'BYTECODE_SUFFIXES',
    'EXTENSION_SUFFIXES',
    'DEBUG_BYTECODE_SUFFIXES',
    'OPTIMIZED_BYTECODE_SUFFIXES',
    'MAGIC_NUMBER',
    'all_suffixes',
]
