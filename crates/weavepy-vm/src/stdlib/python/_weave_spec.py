"""RFC 0053 WS2 — lazy ``__spec__``/``__loader__`` for native imports.

The Rust importer loads modules long before ``importlib`` is
importable, so it cannot construct PEP 451 specs eagerly. Instead,
a module missing ``__spec__``/``__loader__`` resolves them through
this helper on first attribute read (the same bootstrapping dance
CPython plays with ``_frozen_importlib``), and the result is cached
back into the module's dict.

The loader taxonomy matches CPython's:

- no filename                  -> ``BuiltinImporter`` (origin ``'built-in'``)
- ``<frozen name>`` filename   -> ``FrozenImporter`` (origin ``'frozen'``)
- extension-module filename    -> ``ExtensionFileLoader``
- real ``.py`` path            -> ``SourceFileLoader``
- ``__main__`` / ``<string>``  -> spec ``None`` (CPython script semantics)
"""


def make_spec_and_loader(name, filename, is_package, search_locations):
    from importlib.machinery import (
        BuiltinImporter,
        ExtensionFileLoader,
        FrozenImporter,
        ModuleSpec,
        SourceFileLoader,
    )

    if name == "__main__":
        # A real script gets a SourceFileLoader; `-c` / `<stdin>` /
        # the REPL get BuiltinImporter, matching CPython's
        # `_PyImport_AddModule`-created `__main__`. Spec stays None.
        if filename and not filename.startswith("<"):
            loader = SourceFileLoader(name, filename)
        else:
            loader = BuiltinImporter
        return None, loader
    if filename is None:
        if is_package and search_locations:
            # No file but real search locations: a PEP 420 namespace
            # package the native importer assembled. CPython 3.12+
            # gives these a NamespaceLoader (importlib.resources
            # depends on its get_resource_reader — NamespaceDiskTests).
            # Build it the way `_bootstrap._init_module_attrs` does —
            # `__new__` plus a `_path` alias onto the module's own
            # search locations — so `spec.submodule_search_locations`
            # *is* `module.__path__` (the dynamic `_NamespacePath`
            # object included; bpo-32303 asserts loader consistency).
            from importlib.machinery import NamespaceLoader
            loader = NamespaceLoader.__new__(NamespaceLoader)
            loader._path = search_locations
            spec = ModuleSpec(name, loader, origin=None, is_package=True)
            spec.submodule_search_locations = search_locations
            return spec, loader
        spec = ModuleSpec(name, BuiltinImporter, origin="built-in")
        return spec, BuiltinImporter
    if filename.startswith("<"):
        if filename.startswith("<frozen"):
            # Prefer the finder so the spec carries CPython's
            # `loader_state` (origname/filename) — falls back to a bare
            # spec when the frozen-tests override hides the name.
            spec = FrozenImporter.find_spec(name)
            if spec is None:
                spec = ModuleSpec(name, FrozenImporter, origin="frozen",
                                  is_package=is_package)
            return spec, FrozenImporter
        return None, None
    try:
        import _imp
        is_frozen_stdlib = _imp.is_frozen(name)
    except Exception:
        is_frozen_stdlib = False
    if is_frozen_stdlib:
        # CPython 3.13 deep-freezes the startup stdlib (os, io, abc,
        # codecs, site, …): those modules' specs carry
        # `FrozenImporter` + `origin='frozen'` + a populated
        # `loader_state` even though `__file__` points at the real
        # source. `FrozenImporter.find_spec` builds exactly that shape
        # (and `_bootstrap._setup` re-runs — e.g. test_importlib's
        # source-variant importlib re-imports — assert `loader_state`
        # is complete).
        spec = FrozenImporter.find_spec(name)
        if spec is not None:
            return spec, FrozenImporter
    if filename.endswith((".so", ".pyd", ".dylib")):
        loader = ExtensionFileLoader(name, filename)
        spec = ModuleSpec(name, loader, origin=filename,
                          is_package=is_package)
        spec.has_location = True
        return spec, loader
    loader = SourceFileLoader(name, filename)
    spec = ModuleSpec(name, loader, origin=filename, is_package=is_package)
    spec.has_location = True
    if is_package and search_locations:
        spec.submodule_search_locations = list(search_locations)
    try:
        from importlib.util import cache_from_source
        spec.cached = cache_from_source(filename)
    except Exception:
        pass
    return spec, loader


def make_namespace_path(name, portions):
    """A live ``_NamespacePath`` for a native namespace package.

    Recomputes when the parent path changes or ``invalidate_caches()``
    bumps the epoch — test_namespace_pkgs' DynamicPathCalculation and
    SeparatedNamespacePackagesCreatedWhileRunning assert both flavours.
    """
    from importlib import _bootstrap_external as ext
    return ext._NamespacePath(name, portions, ext.PathFinder._get_spec)
