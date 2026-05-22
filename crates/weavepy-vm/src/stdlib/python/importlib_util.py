"""``importlib.util`` — helpers around the spec / loader machinery."""

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
    'LazyLoader',
]


def _cache_tag():
    impl = sys.implementation
    return getattr(impl, 'cache_tag', 'weavepy-3.13')


def cache_from_source(path, debug_override=None, *, optimization=None):
    """Map ``<dir>/<name>.py`` → ``<dir>/__pycache__/<name>.<tag>.pyc``.
    """
    head, tail = os.path.split(path)
    name, _ = os.path.splitext(tail)
    tag = _cache_tag()
    return os.path.join(head, '__pycache__', '{}.{}.pyc'.format(name, tag))


def source_from_cache(path):
    """Reverse of :func:`cache_from_source`."""
    head, tail = os.path.split(path)
    name, _ = os.path.splitext(tail)
    base = name.rsplit('.', 1)[0]
    parent = os.path.dirname(head)
    return os.path.join(parent, base + '.py')


def decode_source(source_bytes):
    """Decode a UTF-8 source-byte blob to text."""
    if isinstance(source_bytes, str):
        return source_bytes
    return source_bytes.decode('utf-8')


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
    """Compose a ``ModuleSpec`` directly from a file path."""
    if loader is None:
        if location and location.endswith('.py'):
            loader = _machinery.SourceFileLoader(name, location)
        elif location and location.endswith('.pyc'):
            loader = _machinery.SourcelessFileLoader(name, location)
        elif location and any(location.endswith(s)
                                for s in _machinery.EXTENSION_SUFFIXES):
            loader = _machinery.ExtensionFileLoader(name, location)
        else:
            loader = _machinery.SourceFileLoader(name, location)
    spec = _machinery.ModuleSpec(name, loader, origin=location,
                                  is_package=bool(submodule_search_locations))
    if submodule_search_locations is not None:
        spec.submodule_search_locations = list(submodule_search_locations)
    return spec


def module_from_spec(spec):
    """Manufacture a fresh module object for ``spec``."""
    import types
    mod = types.ModuleType(spec.name)
    mod.__spec__ = spec
    if spec.origin is not None:
        mod.__file__ = spec.origin
    if spec.is_package:
        mod.__path__ = list(spec.submodule_search_locations or [])
    mod.__loader__ = spec.loader
    mod.__package__ = spec.parent
    return mod


def find_spec(name, package=None):
    """Walk ``sys.meta_path`` looking for ``name``."""
    if name in sys.modules:
        m = sys.modules[name]
        return getattr(m, '__spec__', None)
    for finder in sys.meta_path:
        try:
            spec = finder.find_spec(name, package)
        except Exception:
            spec = None
        if spec is not None:
            return spec
    # Last resort: PathFinder over sys.path.
    return _machinery.PathFinder.find_spec(name)


class LazyLoader:
    """Wrap a loader so the module body runs only on first attribute
    access. Useful for "heavy" optional dependencies.
    """

    def __init__(self, loader):
        self.loader = loader

    @classmethod
    def factory(cls, loader_cls):
        def factory(*args, **kwargs):
            return cls(loader_cls(*args, **kwargs))
        return factory

    def create_module(self, spec):
        return self.loader.create_module(spec)

    def exec_module(self, module):
        # We don't actually defer: eager execution is good enough.
        self.loader.exec_module(module)
