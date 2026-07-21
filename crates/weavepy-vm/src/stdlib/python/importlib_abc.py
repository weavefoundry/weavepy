"""Abstract base classes for the import system.

These are the canonical ABCs ``pip``, ``setuptools``, and
``importlib.metadata`` subclass / `isinstance`-check. We provide
the documented surface as plain Python classes — the abstract
methods raise ``NotImplementedError`` when called directly.
"""

import abc


class Loader(abc.ABC):
    """Base loader. Implementors must provide ``create_module`` /
    ``exec_module`` (or, historically, ``load_module``).
    """

    def create_module(self, spec):
        return None

    def exec_module(self, module):
        raise NotImplementedError

    def load_module(self, fullname):
        spec = getattr(self, 'spec', None)
        if spec is None:
            raise ImportError("loader has no spec", name=fullname)
        module = self.create_module(spec)
        if module is None:
            import types
            module = types.ModuleType(spec.name)
        self.exec_module(module)
        return module


class Finder(abc.ABC):
    """Marker base — superseded by ``MetaPathFinder`` /
    ``PathEntryFinder``.
    """


class MetaPathFinder(Finder):
    def find_spec(self, fullname, path=None, target=None):
        raise NotImplementedError

    def invalidate_caches(self):
        pass


class PathEntryFinder(Finder):
    def find_spec(self, fullname, target=None):
        raise NotImplementedError

    def invalidate_caches(self):
        pass


class ResourceLoader(Loader):
    @abc.abstractmethod
    def get_data(self, path):
        raise NotImplementedError


class InspectLoader(Loader):
    def is_package(self, fullname):
        raise ImportError(name=fullname)

    def get_code(self, fullname):
        source = self.get_source(fullname)
        if source is None:
            return None
        return compile(source, '<string>', 'exec')

    def get_source(self, fullname):
        raise NotImplementedError


class ExecutionLoader(InspectLoader):
    @abc.abstractmethod
    def get_filename(self, fullname):
        raise NotImplementedError


class FileLoader(ResourceLoader, ExecutionLoader):
    def __init__(self, fullname, path):
        self.name = fullname
        self.path = path

    def get_filename(self, fullname=None):
        return self.path

    def get_data(self, path):
        with open(path, 'rb') as f:
            return f.read()


class SourceLoader(FileLoader):
    def get_source(self, fullname=None):
        from importlib.util import decode_source
        return decode_source(self.get_data(self.path))


# Canonical home since 3.11 is `importlib.resources.abc`; these names
# are re-exports so isinstance checks agree across both import paths
# (CPython does exactly this until the 3.14 removal).
from importlib.resources.abc import (  # noqa: E402
    ResourceReader,
    Traversable,
    TraversableResources,
)


__all__ = [
    'Loader',
    'Finder',
    'MetaPathFinder',
    'PathEntryFinder',
    'ResourceLoader',
    'InspectLoader',
    'ExecutionLoader',
    'FileLoader',
    'SourceLoader',
    'ResourceReader',
    'Traversable',
    'TraversableResources',
]
