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
        spec = ModuleSpec(name, BuiltinImporter, origin="built-in")
        return spec, BuiltinImporter
    if filename.startswith("<"):
        if filename.startswith("<frozen"):
            spec = ModuleSpec(name, FrozenImporter, origin="frozen",
                              is_package=is_package)
            return spec, FrozenImporter
        return None, None
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
