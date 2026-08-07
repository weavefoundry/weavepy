"""``importlib._bootstrap`` — WeavePy façade.

In CPython this is the frozen core of the import system; ``importlib``
itself aliases it (``importlib._bootstrap = _bootstrap``). WeavePy's
import core lives in Rust, so this module exposes the handful of
bootstrap entry points stdlib code reaches for directly (notably
``pydoc.importfile`` calling ``_bootstrap._load(spec)``), implemented
over the same spec/loader machinery as ``importlib.util``.
"""

import sys

from importlib.util import module_from_spec as _module_from_spec

__all__ = ['_load', 'spec_from_loader', 'ModuleSpec']

from importlib.util import spec_from_loader
from importlib.machinery import ModuleSpec


def _verbose_message(message, *args, verbosity=1):
    """Print *message* to stderr when `python -v` is active
    (CPython gates on `sys.flags.verbose`)."""
    if getattr(sys.flags, 'verbose', 0) >= verbosity:
        if not message.startswith(('#', 'import ')):
            message = '# ' + message
        print(message.format(*args), file=sys.stderr)


def _module_repr(module):
    """CPython's module repr logic (`_bootstrap._module_repr`): the
    module type's `__repr__` delegates here. The spec is authoritative;
    the fallbacks cover hand-built `types.ModuleType(...)` objects
    (test_module's repr matrix).
    """
    loader = getattr(module, '__loader__', None)
    if spec := getattr(module, "__spec__", None):
        return _module_repr_from_spec(spec, module)

    # Fall through to a catch-all which always succeeds.
    try:
        name = module.__name__
    except AttributeError:
        name = '?'
    try:
        filename = module.__file__
    except AttributeError:
        if loader is None:
            return f'<module {name!r}>'
        else:
            return f'<module {name!r} ({loader!r})>'
    else:
        return f'<module {name!r} from {filename!r}>'


def _module_repr_from_spec(spec, module):
    """Return the repr to use for the module (CPython verbatim, with
    NamespaceLoader imported from its WeavePy home)."""
    name = '?' if spec.name is None else spec.name
    if spec.origin is None:
        loader = spec.loader
        if loader is None:
            return f'<module {name!r}>'
        from importlib.machinery import NamespaceLoader
        if isinstance(loader, NamespaceLoader):
            return f'<module {name!r} (namespace) from {list(loader._path)!r}>'
        else:
            return f'<module {name!r} ({loader!r})>'
    else:
        if spec.has_location:
            return f'<module {name!r} from {spec.origin!r}>'
        else:
            return f'<module {name!r} ({spec.origin})>'


def _load_module_shim(self, fullname):
    """Load the specified module into sys.modules and return it.

    CPython keeps this as the compatibility shim behind every legacy
    ``loader.load_module()`` API (zipimporter's included).
    """
    spec = spec_from_loader(fullname, self)
    if fullname in sys.modules:
        module = sys.modules[fullname]
        try:
            if spec.loader is not None:
                spec.loader.exec_module(module)
        except BaseException:
            try:
                del sys.modules[fullname]
            except KeyError:
                pass
            raise
        return sys.modules.get(fullname, module)
    return _load(spec)


def _init_module_attrs(spec, module, *, override=False):
    """Set the import-system attributes on *module* from *spec*
    (the subset of CPython's `_bootstrap._init_module_attrs` that
    `_exec`/`module_from_spec` consumers observe)."""
    try:
        if override or getattr(module, '__spec__', None) is None:
            module.__spec__ = spec
    except AttributeError:
        pass
    try:
        if override or getattr(module, '__loader__', None) is None:
            module.__loader__ = spec.loader
    except AttributeError:
        pass
    if override or not hasattr(module, '__name__'):
        try:
            module.__name__ = spec.name
        except AttributeError:
            pass
    try:
        module.__package__ = spec.parent
    except AttributeError:
        pass
    if spec.submodule_search_locations is not None:
        try:
            module.__path__ = spec.submodule_search_locations
        except AttributeError:
            pass
    if spec.has_location:
        if spec.origin is not None:
            try:
                module.__file__ = spec.origin
            except AttributeError:
                pass
        if spec.cached is not None:
            try:
                module.__cached__ = spec.cached
            except AttributeError:
                pass
    return module


def _exec(spec, module):
    """Execute the spec's specified module in an existing module's
    namespace (CPython `_bootstrap._exec`; `importlib.reload` rides
    this)."""
    name = spec.name
    if sys.modules.get(name) is not module:
        msg = f'module {name!r} not in sys.modules'
        raise ImportError(msg, name=name)
    try:
        if spec.loader is None:
            if spec.submodule_search_locations is None:
                raise ImportError('missing loader', name=spec.name)
            # Namespace package.
            _init_module_attrs(spec, module, override=True)
        else:
            _init_module_attrs(spec, module, override=True)
            if not hasattr(spec.loader, 'exec_module'):
                import warnings
                warnings.warn(
                    f"{type(spec.loader).__name__}.exec_module() not found; "
                    "falling back to load_module()", ImportWarning)
                spec.loader.load_module(name)
            else:
                spec.loader.exec_module(module)
    finally:
        # Update the order of insertion into sys.modules for module
        # clean-up at shutdown.
        module = sys.modules.pop(spec.name)
        sys.modules[spec.name] = module
    return module


def _load(spec):
    """Create, register, and execute the module described by *spec*.

    Mirrors CPython's `_bootstrap._load`: the module is inserted into
    ``sys.modules`` *before* execution (so circular imports during exec
    see the partial module) and removed again if execution fails.
    """
    module = _module_from_spec(spec)
    sys.modules[spec.name] = module
    try:
        if spec.loader is not None:
            spec.loader.exec_module(module)
    except BaseException:
        try:
            del sys.modules[spec.name]
        except KeyError:
            pass
        raise
    # An import hook may have replaced the entry; honour what's there,
    # like CPython does.
    return sys.modules.get(spec.name, module)
