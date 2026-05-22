"""``importlib.machinery`` — finders, loaders, and module specs.

This is the "low-level" import API. ``pip``, ``setuptools``,
``pluggy``, and most of the packaging ecosystem reach into this
module to construct ``ModuleSpec`` objects, instantiate
``SourceFileLoader`` for plain ``.py`` files, or register
``MetaPathFinder`` subclasses on ``sys.meta_path``.
"""

import os
import sys
import marshal

SOURCE_SUFFIXES = ['.py']
BYTECODE_SUFFIXES = ['.pyc']
EXTENSION_SUFFIXES = ['.so', '.dylib', '.pyd']
DEBUG_BYTECODE_SUFFIXES = BYTECODE_SUFFIXES
OPTIMIZED_BYTECODE_SUFFIXES = BYTECODE_SUFFIXES

# Magic bytes used by the WeavePy ``__pycache__`` writer. Kept in
# sync with ``crates/weavepy-vm/src/pycache.rs``.
MAGIC_NUMBER = b'WPY0'


def all_suffixes():
    return SOURCE_SUFFIXES + BYTECODE_SUFFIXES + EXTENSION_SUFFIXES


class ModuleSpec:
    """PEP 451 module spec.

    Carries the name, loader, origin (path), and a handful of
    metadata fields the import system uses to construct the module
    object. We honour the canonical attribute set but skip the deep
    ``submodule_search_locations`` / ``loader_state`` plumbing that
    only matters for namespace packages.
    """

    def __init__(self, name, loader, *, origin=None, is_package=False):
        self.name = name
        self.loader = loader
        self.origin = origin
        self.submodule_search_locations = [] if is_package else None
        self.loader_state = None
        self.cached = None
        self.parent = name.rpartition('.')[0]
        self.has_location = origin is not None

    @property
    def is_package(self):
        return self.submodule_search_locations is not None

    def __repr__(self):
        parts = ['name={!r}'.format(self.name)]
        if self.loader is not None:
            parts.append('loader={!r}'.format(self.loader))
        if self.origin is not None:
            parts.append('origin={!r}'.format(self.origin))
        return 'ModuleSpec({})'.format(', '.join(parts))


class _SourceFileLoaderBase:
    """Shared shell — concrete subclasses live below."""

    def __init__(self, fullname, path):
        self.name = fullname
        self.path = path

    def __eq__(self, other):
        return (type(self) is type(other) and self.name == other.name
                and self.path == other.path)

    def __hash__(self):
        return hash((type(self).__name__, self.name, self.path))

    def get_filename(self, fullname=None):
        return self.path

    def is_package(self, fullname=None):
        if self.path is None:
            return False
        base = os.path.basename(self.path)
        return base == '__init__.py' or base.startswith('__init__.')

    def get_source(self, fullname=None):
        if not self.path:
            return None
        try:
            with open(self.path, 'rb') as f:
                return f.read().decode('utf-8')
        except OSError:
            return None

    def create_module(self, spec):
        # Use the default object created by the import system.
        return None

    def exec_module(self, module):
        source = self.get_source()
        if source is None:
            raise ImportError("no source for {!r}".format(self.name),
                              name=self.name, path=self.path)
        code = compile(source, self.path or '<frozen>', 'exec')
        exec(code, module.__dict__)


class SourceFileLoader(_SourceFileLoaderBase):
    """Load a module from a ``.py`` file on disk."""


class SourcelessFileLoader(_SourceFileLoaderBase):
    """Load a module from a ``.pyc`` file (no source available).

    Used by tooling that ships compiled-only distributions. We read
    the WeavePy ``__pycache__`` header and unmarshal the embedded
    code object.
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


class ExtensionFileLoader(_SourceFileLoaderBase):
    """Stub for C-extension loaders. WeavePy does not yet load
    ``.so`` files, so calling ``exec_module`` raises ``ImportError``
    with a clear message.
    """

    def get_source(self, fullname=None):
        return None

    def is_package(self, fullname=None):
        return False

    def exec_module(self, module):
        raise ImportError(
            "WeavePy cannot load native extension {!r} (RFC TBD)".format(
                self.path), name=self.name, path=self.path)


class FileFinder:
    """Walk a single directory looking for any of ``loaders``."""

    def __init__(self, path, *loader_details):
        self.path = path
        # Each entry is (loader_cls, [suffixes]).
        self._loaders = list(loader_details)

    @classmethod
    def path_hook(cls, *loader_details):
        def hook(path):
            if not os.path.isdir(path):
                raise ImportError(
                    "only directories are supported", path=path)
            return cls(path, *loader_details)
        return hook

    def invalidate_caches(self):
        pass

    def find_spec(self, fullname, target=None):
        tail = fullname.rpartition('.')[2]
        try:
            entries = os.listdir(self.path or '.')
        except OSError:
            return None
        for loader_cls, suffixes in self._loaders:
            # Package: <dir>/<tail>/__init__<suffix>
            pkg_dir = os.path.join(self.path, tail)
            if os.path.isdir(pkg_dir):
                for sfx in suffixes:
                    init = os.path.join(pkg_dir, '__init__' + sfx)
                    if os.path.isfile(init):
                        loader = loader_cls(fullname, init)
                        spec = ModuleSpec(fullname, loader,
                                           origin=init, is_package=True)
                        spec.submodule_search_locations = [pkg_dir]
                        return spec
            for sfx in suffixes:
                cand = tail + sfx
                if cand in entries:
                    p = os.path.join(self.path, cand)
                    loader = loader_cls(fullname, p)
                    return ModuleSpec(fullname, loader, origin=p)
        return None


class PathFinder:
    """Walk ``sys.path`` looking for ``fullname``."""

    @classmethod
    def invalidate_caches(cls):
        pass

    @classmethod
    def find_spec(cls, fullname, path=None, target=None):
        if path is None:
            path = sys.path
        details = [
            (SourceFileLoader, SOURCE_SUFFIXES),
            (SourcelessFileLoader, BYTECODE_SUFFIXES),
            (ExtensionFileLoader, EXTENSION_SUFFIXES),
        ]
        for entry in path:
            if not entry:
                entry = '.'
            try:
                finder = FileFinder(entry, *details)
                spec = finder.find_spec(fullname, target)
                if spec is not None:
                    return spec
            except (OSError, ImportError):
                continue
        return None


class BuiltinImporter:
    """Spec lookup for modules registered in
    ``sys.builtin_module_names``.
    """

    @classmethod
    def find_spec(cls, fullname, path=None, target=None):
        if fullname in sys.builtin_module_names:
            return ModuleSpec(fullname, cls, origin='built-in')
        return None

    @classmethod
    def create_module(cls, spec):
        if spec.name in sys.modules:
            return sys.modules[spec.name]
        return None

    @classmethod
    def exec_module(cls, module):
        # The actual loading happens in the host VM; if the module is
        # already in sys.modules we have nothing left to do here.
        pass


class FrozenImporter:
    """Spec lookup for the frozen-Python stdlib bundle baked into
    the WeavePy binary.
    """

    @classmethod
    def find_spec(cls, fullname, path=None, target=None):
        # We don't (yet) expose the frozen registry through Python;
        # treat any non-builtin known to ``sys.modules`` as a hit.
        if fullname in sys.modules:
            return ModuleSpec(fullname, cls, origin='frozen')
        return None

    @classmethod
    def create_module(cls, spec):
        return sys.modules.get(spec.name)

    @classmethod
    def exec_module(cls, module):
        pass


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
