"""Abstract base classes related to import (CPython 3.13 surface).

These are the canonical ABCs ``pip``, ``setuptools``, and
``importlib.metadata`` subclass / `isinstance`-check. Following
CPython's ``importlib/abc.py``, the concrete machinery classes are
*registered* against the ABCs (so
``isinstance(SourceFileLoader(...), importlib.abc.SourceLoader)``
holds), the finder ABCs deliberately do *not* define ``find_spec``
(back-compat ``hasattr`` probes depend on its absence), and the
``importlib.resources.abc`` names resolve lazily with a deprecation
warning, exactly like CPython until the 3.14 removal.
"""

from importlib import machinery
import abc
import warnings

from importlib.resources import abc as _resources_abc


__all__ = [
    'Loader', 'MetaPathFinder', 'PathEntryFinder',
    'ResourceLoader', 'InspectLoader', 'ExecutionLoader',
    'FileLoader', 'SourceLoader',
]


def __getattr__(name):
    """
    For backwards compatibility, continue to make names
    from _resources_abc available through this module. #93963
    """
    if name in _resources_abc.__all__:
        obj = getattr(_resources_abc, name)
        warnings._deprecated(f"{__name__}.{name}", remove=(3, 14))
        globals()[name] = obj
        return obj
    raise AttributeError(f'module {__name__!r} has no attribute {name!r}')


def _register(abstract_cls, *classes):
    for cls in classes:
        abstract_cls.register(cls)


class Loader(metaclass=abc.ABCMeta):

    """Abstract base class for import loaders."""

    def create_module(self, spec):
        """Return a module to initialize and into which to load.

        This method should raise ImportError if anything prevents it
        from creating a new module.  It may return None to indicate
        that the spec should create the new module.
        """
        return None

    # We don't define exec_module() here since that would break
    # hasattr checks we do to support backward compatibility.

    def load_module(self, fullname):
        """Return the loaded module.

        This method is deprecated in favor of loader.exec_module(). If
        exec_module() exists then it is used to provide a
        backwards-compatible functionality for this method.
        """
        if not hasattr(self, 'exec_module'):
            raise ImportError
        import importlib._bootstrap
        return importlib._bootstrap._load_module_shim(self, fullname)


class MetaPathFinder(metaclass=abc.ABCMeta):

    """Abstract base class for import finders on sys.meta_path."""

    # We don't define find_spec() here since that would break
    # hasattr checks we do to support backward compatibility.

    def invalidate_caches(self):
        """An optional method for clearing the finder's cache, if any.
        This method is used by importlib.invalidate_caches().
        """

_register(MetaPathFinder, machinery.BuiltinImporter,
          machinery.FrozenImporter, machinery.PathFinder)


class PathEntryFinder(metaclass=abc.ABCMeta):

    """Abstract base class for path entry finders used by PathFinder."""

    def invalidate_caches(self):
        """An optional method for clearing the finder's cache, if any.
        This method is used by PathFinder.invalidate_caches().
        """

_register(PathEntryFinder, machinery.FileFinder)


class ResourceLoader(Loader):

    """Abstract base class for loaders which can return data from their
    back-end storage."""

    @abc.abstractmethod
    def get_data(self, path):
        """Abstract method which when implemented should return the bytes for
        the specified path.  The path must be a str."""
        raise OSError


class InspectLoader(Loader):

    """Abstract base class for loaders which support inspection about the
    modules they can load."""

    def is_package(self, fullname):
        """Optional method which when implemented should return whether the
        module is a package.  The fullname is a str.  Returns a bool.

        Raises ImportError if the module cannot be found.
        """
        raise ImportError

    def get_code(self, fullname):
        """Method which returns the code object for the module.

        The fullname is a str.  Returns a types.CodeType if possible, else
        returns None if a code object does not make sense
        (e.g. built-in module). Raises ImportError if the module cannot be
        found.
        """
        source = self.get_source(fullname)
        if source is None:
            return None
        return self.source_to_code(source)

    @abc.abstractmethod
    def get_source(self, fullname):
        """Abstract method which should return the source code for the
        module.  The fullname is a str.  Returns a str.

        Raises ImportError if the module cannot be found.
        """
        raise ImportError

    @staticmethod
    def source_to_code(data, path='<string>'):
        """Compile 'data' into a code object.

        The 'data' argument can be anything that compile() can handle. The
        'path' argument should be where the data was retrieved (when
        applicable)."""
        return compile(data, path, 'exec', dont_inherit=True)

    def exec_module(self, module):
        code = self.get_code(module.__name__)
        if code is None:
            raise ImportError(
                f'cannot load module {module.__name__!r} when '
                'get_code() returns None')
        exec(code, module.__dict__)

    def load_module(self, fullname):
        import importlib._bootstrap
        return importlib._bootstrap._load_module_shim(self, fullname)

_register(InspectLoader, machinery.BuiltinImporter,
          machinery.FrozenImporter, machinery.NamespaceLoader)


class ExecutionLoader(InspectLoader):

    """Abstract base class for loaders that wish to support the execution of
    modules as scripts."""

    @abc.abstractmethod
    def get_filename(self, fullname):
        """Abstract method which should return the value that __file__ is to
        be set to.

        Raises ImportError if the module cannot be found.
        """
        raise ImportError

    def get_code(self, fullname):
        """Method to return the code object for fullname.

        Should return None if not applicable (e.g. built-in module).
        Raise ImportError if the module cannot be found.
        """
        source = self.get_source(fullname)
        if source is None:
            return None
        try:
            path = self.get_filename(fullname)
        except ImportError:
            return self.source_to_code(source)
        else:
            return self.source_to_code(source, path)

_register(ExecutionLoader, machinery.ExtensionFileLoader,
          machinery.AppleFrameworkLoader)


class FileLoader(ResourceLoader, ExecutionLoader):

    """Abstract base class partially implementing the ResourceLoader and
    ExecutionLoader ABCs."""

    def __init__(self, fullname, path):
        self.name = fullname
        self.path = path

    def get_filename(self, fullname=None):
        return self.path

    def get_data(self, path):
        with open(path, 'rb') as f:
            return f.read()

_register(FileLoader, machinery.SourceFileLoader,
          machinery.SourcelessFileLoader)


class SourceLoader(FileLoader):

    """Abstract base class for loading source code (and optionally any
    corresponding bytecode).

    To support loading from source code, the abstractmethods inherited from
    ResourceLoader and ExecutionLoader need to be implemented. To also support
    loading from bytecode, the optional methods specified directly by this ABC
    is required.

    Inherited abstractmethods not implemented in this ABC:

        * ResourceLoader.get_data
        * ExecutionLoader.get_filename
    """

    def path_mtime(self, path):
        """Return the (int) modification time for the path (str)."""
        if self.path_stats.__func__ is SourceLoader.path_stats:
            raise OSError
        return int(self.path_stats(path)['mtime'])

    def path_stats(self, path):
        """Return a metadata dict for the source pointed to by the path (str).
        Possible keys:
        - 'mtime' (mandatory) is the numeric timestamp of last source
          file modification;
        - 'size' (optional) is the size in bytes of the source code.
        """
        if self.path_mtime.__func__ is SourceLoader.path_mtime:
            raise OSError
        return {'mtime': self.path_mtime(path)}

    def set_data(self, path, data):
        """Write the bytes to the path (if possible).

        Any needed intermediary directories are to be created. If for some
        reason the file cannot be written because of permissions, fail
        silently.
        """

    def get_source(self, fullname=None):
        from importlib.util import decode_source
        path = self.get_filename(fullname)
        try:
            source_bytes = self.get_data(path)
        except OSError as exc:
            raise ImportError('source not available through get_data()',
                              name=fullname) from exc
        return decode_source(source_bytes)

_register(SourceLoader, machinery.SourceFileLoader)
