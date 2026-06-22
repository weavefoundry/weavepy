"""``pkgutil`` — package discovery helpers.

This module ships the ~10 functions / classes that the packaging
ecosystem calls into: ``iter_modules``, ``walk_packages``,
``get_data``, ``extend_path``, ``find_loader``, ``ModuleInfo``,
and a tiny ``ImpImporter`` stub for backwards-compat with code
that references it.
"""

import collections
import os
import sys

__all__ = [
    'ModuleInfo',
    'iter_modules',
    'walk_packages',
    'get_importer',
    'iter_importers',
    'get_loader',
    'find_loader',
    'get_data',
    'read_code',
    'extend_path',
    'resolve_name',
    'ImpImporter',
]


def read_code(stream):
    # This helper is needed in order for the PEP 302 emulation to
    # correctly handle compiled files (mirrors CPython's pkgutil.read_code).
    import marshal
    import importlib.machinery

    magic = stream.read(4)
    if magic != importlib.machinery.MAGIC_NUMBER:
        return None

    stream.read(12)  # Skip rest of the header
    return marshal.load(stream)


def get_importer(path_item):
    """Retrieve a finder for the given path item.

    The returned finder is cached in ``sys.path_importer_cache`` if it was
    newly created by a path hook. ``None`` means the path item is not a valid
    ``sys.path`` entry (e.g. a plain file rather than a directory/zipfile)."""
    path_item = os.fsdecode(path_item)
    try:
        importer = sys.path_importer_cache[path_item]
    except KeyError:
        for path_hook in sys.path_hooks:
            try:
                importer = path_hook(path_item)
                sys.path_importer_cache.setdefault(path_item, importer)
                break
            except ImportError:
                pass
        else:
            importer = None
    return importer


def iter_importers(fullname=""):
    """Yield finders for ``fullname`` on ``sys.meta_path`` and ``sys.path``."""
    if fullname.startswith('.'):
        msg = "Relative module name {!r} not supported".format(fullname)
        raise ImportError(msg)
    if '.' in fullname:
        # Get the containing package's __path__
        import importlib
        pkg_name = fullname.rpartition(".")[0]
        pkg = importlib.import_module(pkg_name)
        path = getattr(pkg, '__path__', None)
        if path is None:
            return
    else:
        yield from sys.meta_path
        path = sys.path
    for item in path:
        yield get_importer(item)

ModuleInfo = collections.namedtuple('ModuleInfo', 'module_finder name ispkg')


def _has_module_marker(path):
    """``__init__.py`` (or any of the source/extension equivalents)."""
    for sfx in ('.py', '.pyc'):
        if os.path.isfile(os.path.join(path, '__init__' + sfx)):
            return True
    return False


def iter_modules(path=None, prefix=''):
    """Yield ``ModuleInfo`` for every top-level module in ``path``."""
    if path is None:
        path = sys.path
    seen = set()
    for entry in path:
        if not entry:
            entry = '.'
        try:
            names = os.listdir(entry)
        except OSError:
            continue
        for name in names:
            full = os.path.join(entry, name)
            if os.path.isdir(full) and _has_module_marker(full):
                key = prefix + name
                if key in seen:
                    continue
                seen.add(key)
                yield ModuleInfo(None, prefix + name, True)
            elif name.endswith('.py') and name != '__init__.py':
                key = prefix + name[:-3]
                if key in seen:
                    continue
                seen.add(key)
                yield ModuleInfo(None, prefix + name[:-3], False)


def walk_packages(path=None, prefix='', onerror=None):
    """Recursively walk every package under ``path``."""
    def _seen(p, m={}):
        if p in m:
            return True
        m[p] = True
        return False

    for info in iter_modules(path, prefix):
        yield info
        if not info.ispkg:
            continue
        try:
            __import__(info.name)
        except Exception:
            if onerror is not None:
                onerror(info.name)
            continue
        mod = sys.modules.get(info.name)
        sub_path = getattr(mod, '__path__', None) if mod else None
        if sub_path and not _seen(tuple(sub_path)):
            yield from walk_packages(sub_path, info.name + '.', onerror)


def get_loader(module_or_name):
    """Return the loader for a module name. Falls back to ``None``."""
    if isinstance(module_or_name, str):
        module = sys.modules.get(module_or_name)
        if module is None:
            try:
                module = __import__(module_or_name)
            except ImportError:
                return None
    else:
        module = module_or_name
    return getattr(module, '__loader__', None)


def find_loader(name):
    """Like ``get_loader`` but never imports the module."""
    if name in sys.modules:
        return getattr(sys.modules[name], '__loader__', None)
    from importlib import util
    spec = util.find_spec(name)
    return spec.loader if spec else None


def get_data(package, resource):
    """Read a package data file as bytes."""
    loader = get_loader(package)
    if loader is None or not hasattr(loader, 'get_data'):
        from importlib import resources
        return resources.files(package).joinpath(resource).read_bytes()
    mod = sys.modules[package]
    parts = resource.split('/')
    path = os.path.join(os.path.dirname(mod.__file__), *parts)
    return loader.get_data(path)


def extend_path(path, name):
    """Extend ``__path__`` to include every directory on ``sys.path``
    that contains a sub-package with the same name.

    Mirrors CPython's classic namespace-package recipe.
    """
    if not isinstance(path, list):
        return path
    if '.' not in name:
        return path
    parent_package, _, leaf = name.rpartition('.')
    for dir_name in sys.path:
        if not isinstance(dir_name, str):
            continue
        sub = os.path.join(dir_name, *parent_package.split('.'), leaf)
        init = os.path.join(sub, '__init__.py')
        if os.path.isdir(sub) and os.path.isfile(init) and sub not in path:
            path.append(sub)
    return path


def resolve_name(name):
    """Resolve ``module:attr`` to the attribute."""
    if ':' in name:
        module_name, _, attr_chain = name.partition(':')
    else:
        module_name, _, attr_chain = name.rpartition('.')
    mod = __import__(module_name)
    for part in module_name.split('.')[1:]:
        mod = getattr(mod, part)
    if not attr_chain:
        return mod
    obj = mod
    for part in attr_chain.split('.'):
        obj = getattr(obj, part)
    return obj


class ImpImporter:
    """Deprecated upstream — kept as a compat stub."""

    def __init__(self, path=None):
        self.path = path

    def find_module(self, fullname, path=None):
        return None
