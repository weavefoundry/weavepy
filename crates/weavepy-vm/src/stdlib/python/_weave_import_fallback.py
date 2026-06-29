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

# Sentinel marking the "live module" form of :func:`import_via_finders`'s
# return value. When a finder builds the module itself (PEP 451
# ``create_module``/``exec_module``, no code object) the helper drives that
# protocol here and hands the finished module back under this tag; the Rust
# loader then caches it as ``sys.modules[name]`` verbatim. Kept identical to
# ``LIVE_MODULE_SENTINEL`` on the Rust side. A NUL prefix keeps it from
# colliding with any real module's code object.
_LIVE_MODULE = "\x00weave-live-module"


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
        if spec is None:
            continue
        # WeavePy's *native* loader is authoritative for builtin and frozen
        # modules and has already tried — and failed — to resolve `name` as
        # one before this last-resort fallback runs. But the first time any
        # code touches `importlib` (e.g. `import six`), importlib installs its
        # `BuiltinImporter` / `FrozenImporter` on the (otherwise empty)
        # `sys.meta_path`; those still *claim* such names purely by listing
        # — `_datetime` is in `sys.builtin_module_names` — while
        # `create_module` returns ``None`` and no working module is built in
        # this VM. Skip a builtin/frozen-origin spec so the genuine
        # `ModuleNotFoundError` stands (letting `datetime`'s
        # ``try: from _datetime import * / except ImportError:`` fall back to
        # `_pydatetime`), while still honouring custom finders (six's
        # `six.moves`) and path-based archives (`zipimport`).
        origin = getattr(spec, "origin", None)
        if origin in ("built-in", "frozen"):
            continue
        return spec
    return None


def import_via_finders(name):
    """Resolve *name* via the finders and return the data the Rust loader
    needs to build a *native* module itself.

    Returns ``None`` when no finder claims *name* (the caller raises its own
    ``ModuleNotFoundError``), or when the match is a namespace package the
    native loader handles itself. For a finder that exposes a *code object*
    (``zipimport``, sourceless ``.pyc``) it returns the tuple::

        (code, is_package, filename, submodule_search_locations, loader, spec)

    so the Rust loader can build a first-class *native* module — keeping
    dotted-import parent binding and ``from pkg import sub`` correct. For a
    finder that builds the module *itself* via the PEP 451
    ``create_module``/``exec_module`` protocol (no code object — e.g. six's
    ``_SixMetaPathImporter`` for the virtual ``six.moves``) it drives that
    protocol here and returns the live module under the ``_LIVE_MODULE`` tag::

        (_LIVE_MODULE, module)

    Errors raised while loading a *found* module are genuine import failures
    and propagate.
    """
    spec = find_spec_for(name)
    if spec is None:
        return None
    loader = spec.loader
    if loader is None:
        return None
    get_code = getattr(loader, "get_code", None)
    code = get_code(name) if get_code is not None else None
    if code is not None:
        locations = spec.submodule_search_locations
        is_package = locations is not None
        return (code, is_package, spec.origin,
                list(locations) if is_package else None, loader, spec)
    # No code object: a finder that constructs the module itself. Drive the
    # PEP 451 create_module/exec_module protocol and hand the result back.
    return (_LIVE_MODULE, _build_dynamic(spec, name, loader))


def _build_dynamic(spec, name, loader):
    """Construct *name* via the PEP 451 protocol of its *loader* and return the
    live module (already registered in ``sys.modules``).

    Mirrors the relevant slice of ``importlib._bootstrap._load``: honour an
    already-present ``sys.modules`` entry (reload / circular import), let the
    loader build the object (``create_module``), set the import-system
    attributes, register *before* executing so a circular import sees the
    partial module, then run ``exec_module``. On failure the half-built entry
    is removed so a retry starts clean.
    """
    existing = sys.modules.get(name)
    if existing is not None:
        return existing
    create_module = getattr(loader, "create_module", None)
    module = create_module(spec) if create_module is not None else None
    if module is None:
        import types
        module = types.ModuleType(name)
    # PEP 451 _init_module_attrs (the subset finders depend on). Some loaders
    # (six's) hand back a pre-built module whose attributes are already set;
    # assigning again is harmless, and `__path__` is only added when the spec
    # says it is a package and the module hasn't declared its own.
    try:
        module.__name__ = name
    except Exception:
        pass
    try:
        module.__loader__ = loader
        module.__spec__ = spec
        if (spec.submodule_search_locations is not None
                and not hasattr(module, "__path__")):
            module.__path__ = spec.submodule_search_locations
    except Exception:
        pass
    sys.modules[name] = module
    exec_module = getattr(loader, "exec_module", None)
    try:
        if exec_module is not None:
            exec_module(module)
    except BaseException:
        sys.modules.pop(name, None)
        raise
    # exec_module may have replaced the entry (rare); return whatever is bound.
    return sys.modules.get(name, module)
