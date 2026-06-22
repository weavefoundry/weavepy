"""WeavePy import fallback — bridge from the Rust loader to ``sys.meta_path``.

WeavePy's primary import loader is Rust-native and resolves builtins, frozen
stdlib, C extensions, and on-disk ``.py`` files directly. Module *kinds* it
doesn't understand natively — most importantly modules inside ZIP archives on
``sys.path`` (``zipimport``) and sourceless ``.pyc`` files reached through a
custom finder — are handled here: when the native loader misses, it calls
:func:`import_via_finders`, which walks ``sys.meta_path`` exactly as CPython's
``__import__`` does and loads the located spec.

The parent package (if any) is always imported by the Rust loader *before* the
child, so by the time this runs ``sys.modules[parent].__path__`` is available
to scope the finder search — mirroring CPython's ``_find_spec`` contract.
"""

import sys


def find_spec_for(name):
    """Locate *name* through ``sys.meta_path``; return its spec or ``None``.

    The parent package is already imported by the Rust loader, so when *name*
    is dotted the search is scoped to ``sys.modules[parent].__path__`` exactly
    as CPython's ``_find_spec`` does.
    """
    parent = name.rpartition(".")[0]
    if parent:
        parent_module = sys.modules.get(parent)
        if parent_module is None:
            return None
        parent_path = getattr(parent_module, "__path__", None)
        if parent_path is None:
            # Parent is not a package — there are no submodules to find.
            return None
    else:
        parent_path = None

    for finder in list(sys.meta_path):
        find_spec = getattr(finder, "find_spec", None)
        if find_spec is None:
            continue
        try:
            spec = find_spec(name, parent_path)
        except ImportError:
            spec = None
        if spec is not None:
            return spec
    return None


def import_via_finders(name):
    """Resolve *name* via the finders and return the data the Rust loader
    needs to build a *native* module itself.

    Returns ``None`` when no finder claims *name* (the caller raises its own
    ``ModuleNotFoundError``), or when the match is a namespace package / has
    no code object (the native loader's own namespace handling applies).
    Otherwise returns the tuple::

        (code, is_package, filename, submodule_search_locations, loader, spec)

    Building the module on the Rust side (rather than via
    ``importlib._bootstrap._load`` + ``types.ModuleType``) is what keeps it a
    first-class native module object — so dotted-import parent binding and
    ``from pkg import sub`` resolve it correctly. Errors raised while reading
    the code object are genuine import failures and propagate.
    """
    spec = find_spec_for(name)
    if spec is None:
        return None
    loader = spec.loader
    if loader is None:
        return None
    get_code = getattr(loader, "get_code", None)
    if get_code is None:
        return None
    code = get_code(name)
    if code is None:
        return None
    locations = spec.submodule_search_locations
    is_package = locations is not None
    return (code, is_package, spec.origin,
            list(locations) if is_package else None, loader, spec)
