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


class Traversable(abc.ABC):
    @abc.abstractmethod
    def iterdir(self):
        raise NotImplementedError

    @abc.abstractmethod
    def read_bytes(self):
        raise NotImplementedError

    @abc.abstractmethod
    def read_text(self, encoding='utf-8'):
        raise NotImplementedError

    @abc.abstractmethod
    def is_dir(self):
        raise NotImplementedError

    @abc.abstractmethod
    def is_file(self):
        raise NotImplementedError

    @abc.abstractmethod
    def joinpath(self, *child):
        raise NotImplementedError

    def __truediv__(self, other):
        return self.joinpath(other)

    @property
    def name(self):
        raise NotImplementedError

    def open(self, mode='r', *args, **kwargs):
        if mode == 'r':
            return open(str(self), encoding=kwargs.get('encoding', 'utf-8'))
        return open(str(self), mode)


class TraversableResources(ResourceLoader):
    @abc.abstractmethod
    def files(self):
        raise NotImplementedError


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
    'Traversable',
    'TraversableResources',
]
