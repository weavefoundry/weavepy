# RFC 0029: `numpy`-grade C extensions — end-to-end import, dtype, ufuncs

- **Status**: Accepted
- **Authors**: WeavePy authors
- **Created**: 2026-05-26
- **Tracking issue**: TBD
- **Supersedes**: §"Future work — RFC 0029 — numpy.so end-to-end" deferred from RFC 0028

## Summary

Take the C-extension type machinery RFC 0028 shipped — the PEP 3118
buffer protocol, PEP 590 vectorcall, the full `PyType_FromSpec[WithBases]`
slot surface — and ride it all the way to a working numpy-grade C
extension. Three threads:

1. **Import machinery.** Wire a real PEP 451 import system:
   `sys.meta_path` with `BuiltinImporter`, `FrozenImporter`, and a
   real `PathFinder`; `sys.path_hooks` with `FileFinder.path_hook`;
   `sys.path_importer_cache`; the full `ModuleSpec`/`ModuleType`
   surface; `importlib.machinery.ExtensionFileLoader` that drives
   our existing dlopen path via a process-global hook. Result:
   `importlib.util.find_spec("anything")` works, `*.so` /
   `*.dylib` / `*.pyd` are discoverable on `sys.path`,
   the importer cache is honoured, and PEP 420 namespace packages
   compose with binary extensions correctly.

2. **C-API tail.** Fill in the private/extended API surface a real
   binary extension needs. RFC 0028 shipped slot tables and buffer
   protocol; RFC 0029 fills in: full `PyArg_ParseTupleAndKeywords`
   keyword binding, the long tail of `PyLong_*` / `PyUnicode_*` /
   `PyList_*` / `PyDict_*` / `PyTuple_*` / `PyBytes_*` helpers,
   datetime C-API via capsule import, complete capsule surface
   (`PyCapsule_Import` / `PyCapsule_SetName` / `PyCapsule_GetContext`
   / `PyCapsule_SetContext` / `PyCapsule_SetDestructor` /
   `PyCapsule_Type`), `PyImport_Import*` + `PyImport_GetModule*`,
   numeric protocol completion (`PyNumber_Index`, `PyNumber_Long`,
   `PyNumber_AsSsize_t`, `PyNumber_InPlace*`), unicode internals
   (`PyUnicode_AsUTF8AndSize`, `PyUnicode_AsEncodedString`,
   `PyUnicode_FromEncodedObject`, `PyUnicode_InternFromString`),
   `_PyObject_LookupAttr`, `_PyObject_GenericGetAttrWithDict`,
   `PyDict_Next`, `PyDict_NextItem`, `_PyDict_GetItemStringWithError`,
   `PyList_GET_ITEM` / `PyList_SET_ITEM` / `PyTuple_GET_ITEM` /
   `PyTuple_SET_ITEM` proper macro behaviour, `PySequence_Fast*`,
   `PyObject_GetIter` / `PyObject_GetIterWithError`,
   `Py_EnterRecursiveCall` / `Py_LeaveRecursiveCall`,
   `PyThreadState_GetDict`, `_PyArg_ParseStackAndKeywords`,
   `_PyEval_GetBuiltin`, `_PyImport_LoadDynamicModuleWithSpec`,
   `PyImport_AddModuleObject`. Total surface added: ~120 new
   symbols.

3. **`_numpylike.c` — a real-shape ndarray extension.** A 1,900-line
   C extension that implements a numpy-shaped subset:
   `ndarray(shape, dtype)` with the full dtype surface (i8, i16,
   i32, i64, u8, u16, u32, u64, f32, f64, bool, complex64,
   complex128), the full buffer protocol with strides and format
   round-trip, vectorcall + `__call__` from a registered `ufunc`,
   broadcasting between shape-compatible arrays, fancy indexing
   with int/bool/slice/tuple keys, reshape + transpose + ravel,
   reduce operations (`sum`, `prod`, `min`, `max`, `mean`),
   element-wise ufuncs (`add`, `subtract`, `multiply`, `divide`,
   `power`, `sqrt`, `exp`, `log`, `abs`, `negative`, `sign`,
   `floor`, `ceil`, `round`, `trunc`), structured dtypes
   (field-based access), C-order and F-order memory layouts,
   `astype` (dtype conversion), `tobytes`/`frombuffer` zero-copy
   round-trips, `array_repr` / `array_str` for printing, the
   datetime C-API consumer pattern, and a `_array_module` capsule
   API that other extensions can import.

Net diff: **~22-28K LOC** (C-API expansion + import machinery +
build harness + bundled `_numpylike.c` + 30+ integration tests +
1 frozen `_numpylike` Python facade + expanded `_minipip` +
RFC doc).

The mission alignment is direct: the project README states
"100% compatible, drop-in replacement for CPython." After this
RFC lands, the README's "Status" line can legitimately read
"with a live numpy-grade extension and a working binary-wheel
import path." The C-extension machinery from RFC 0028 stops
being a hand-curated 14-test fixture and becomes a real,
stress-tested ecosystem entry point.

## Motivation

After RFC 0028, the C-extension *type machinery* matched CPython:
heap types via `PyType_FromSpec`, the full buffer protocol,
vectorcall, dunder shims for every protocol family. What was
still missing — and the reason `import numpy` would not work
end-to-end — was three categories of surface:

1. **The import side.** `importlib.machinery.ExtensionFileLoader`
   didn't exist as a real loader. `sys.meta_path` was unpopulated.
   `sys.path_hooks` was unpopulated. The frozen `importlib.util`
   raised on `find_spec("anything")` because it tried to walk
   `sys.meta_path` (which had no entries) and reach for
   attributes that didn't exist. The fall-back path that *did*
   work (the Rust-side `Interpreter::load_one` walk) bypassed
   the user-visible spec machinery entirely, so any extension
   that introspected `spec.loader` or `__loader__` saw `None`.

2. **The long tail of C-API surface.** RFC 0028's `_ndarray.c`
   fixture was deliberately scoped to exercise the slot table.
   A real numpy-shape extension calls hundreds of helpers RFC
   0028 didn't ship: `PyArg_ParseTupleAndKeywords` with real
   keyword binding (every `np.array(object, dtype=, copy=,
   order=)` call uses this); `PyDict_Next` for walking
   metadata dicts; `PyImport_ImportModule` + `PyObject_GetAttrString`
   for fetching the datetime C-API capsule; `PyCapsule_Import`
   for actually consuming such capsules; `PyUnicode_AsUTF8AndSize`
   for the zero-copy hot path on every string-keyed operation;
   `_PyObject_LookupAttr` (a CPython private-API helper that's
   nonetheless a hard dependency of numpy's compiled extensions);
   `PyNumber_Index` for size-converting argument coercion;
   `PySequence_Fast` for tuple/list-agnostic iteration. The
   list is long; each entry is small; the aggregate is the gap.

3. **A real-shape test fixture.** RFC 0028's `_ndarray.c` is
   ~552 lines and exercises one storage shape with hand-rolled
   dtype handling. A *real* ndarray extension carries a dtype
   object hierarchy, ufunc dispatch, broadcasting rules, fancy
   indexing, structured types, and capsule-based extension
   APIs. Without a fixture exercising those, every one of them
   is a future regression waiting to happen. The new
   `_numpylike.c` is the regression net.

Each individually is small. The aggregate is the milestone:
"WeavePy can host a binary extension shaped like real numpy,
and the import side / C-API tail are CPython-faithful enough
that the extension's source compiles unchanged."

Down-tree, this RFC unblocks:

- **`pip install <binary-wheel>`** for the long tail of native
  packages (`pillow`, `lxml`, `cryptography`, `psutil`, …).
  The binary-wheel installer in `_minipip` now resolves the
  `weavepy-cp313-{darwin,linux,windows}-<arch>` ABI tag and
  unpacks the matching wheel into `site-packages`, with the
  same import-time spec machinery finding the bundled `.so`.

- **`importlib`-grade introspection.** `find_spec(name)`,
  `find_loader(name)`, `spec.loader`, `spec.origin`,
  `spec.submodule_search_locations` — every one of these
  returns the same shape CPython does, so any code that
  introspects modules (`pluggy`, `pytest`'s import-time
  rewrite, `pkg_resources`) sees the right answers.

- **RFC 0030 — actual vendored numpy.** Once this RFC lands,
  a future RFC can vendor numpy's C sources, build them
  against `Python.h`, and gate CI on
  `weavepy -c "import numpy; print(numpy.zeros((3, 3)) @ numpy.ones((3, 3)))"`.
  RFC 0029 *is* the precondition for that work.

## CPython reference

This RFC tracks **CPython 3.13** semantics. Every surface
references a specific behaviour observable in CPython:

- **PEP 451** — *A ModuleSpec Type for the Import System.*
  `ModuleSpec(name, loader, origin=, is_package=,
  loader_state=, submodule_search_locations=)`, the full
  finder protocol (`find_spec(name, path, target=)`), the
  exec_module/create_module two-phase loader contract.

- **PEP 489** — *Multi-phase extension module initialisation.*
  `PyModuleDef_HEAD_INIT`, the slot table for
  `Py_mod_create` / `Py_mod_exec`. Honoured by our loader so
  extensions that opt into multi-phase init work.

- **PEP 587** — *Python Initialization Configuration.* We
  honour the parts that affect import (`PYTHONPATH`,
  `PYTHONHOME`, `PYTHONPLATLIBDIR`) through the existing
  CLI surface; the `_PyConfig_*` C-API is stubbed.

- **PEP 3118** — *Revising the buffer protocol.* RFC 0028's
  surface is unchanged; this RFC's `_numpylike.c` exercises
  the strides + format round-trip end-to-end.

- **`Include/cpython/abstract.h`** — `_PyObject_LookupAttr`,
  `_PyObject_GenericGetAttrWithDict`,
  `_PyObject_CallMethodIdObjArgs`, `_PyObject_GetAttrId`.
  These are CPython-private but numpy reaches for them; we
  ship them.

- **`Include/cpython/dictobject.h`** — `PyDict_Next`,
  `PyDict_NextItem`, `_PyDict_GetItemIdWithError`. Same
  story: marked private in CPython but in practice load-
  bearing for numpy.

- **`Include/datetime.h`** — `PyDateTime_CAPI`,
  `PyDateTimeAPI`, the capsule import + slot table for the
  full datetime constructor surface (`PyDate_FromDate`,
  `PyTime_FromTime`, `PyDateTime_FromDateAndTime`,
  `PyDelta_FromDSU`). We expose the capsule under
  `datetime.datetime_CAPI` so extensions can `PyCapsule_Import`
  it the same way they do under CPython.

- **`Include/cpython/import.h`** — `_PyImport_LoadDynamicModuleWithSpec`,
  `PyImport_AddModuleObject`, the import-lock helpers.

- **`Lib/importlib/_bootstrap_external.py`** — the reference
  shape of `FileFinder`, `_PathFinder`, `ExtensionFileLoader`,
  `SourceFileLoader`. Our frozen `importlib._bootstrap_external`
  mirrors it line-for-line for the surfaces we implement.

We deliberately do **not** track in this RFC:

- **Vendored real numpy.** This RFC ships the machinery + a
  numpy-shaped *fixture*; the next RFC builds real numpy on
  top.
- **PEP 489 multi-phase init for all use cases.** Single-phase
  `PyInit_<modname>` works; multi-phase works for the common
  pattern (no slots beyond `Py_mod_exec`); the weirder slot
  combinations (`Py_mod_multiple_interpreters`,
  `Py_mod_gil_disabled`) are accepted but inert.
- **`pip install` from arbitrary source distributions.**
  `_minipip` handles binary wheels and pure-Python wheels.
  PEP 517 source builds remain out of scope; that's a
  `setuptools` / `build` / `wheel` story.

## Detailed design

The work splits into ten groups, ordered by dependency: each
group builds on the previous one's surface.

### Group 1 — Import spec machinery (`importlib._bootstrap_external`, ~2.5K LOC)

A frozen `importlib._bootstrap_external` module that implements
the full PEP 451 surface:

- `ModuleSpec(name, loader, *, origin=None, loader_state=None, is_package=None)`
  with `submodule_search_locations`, `cached`, `has_location`,
  `parent`. Used as `__spec__` on every loaded module.

- `BuiltinImporter` — wraps the existing built-in-module
  registry; `find_spec("sys")` returns a spec with
  `loader=BuiltinImporter` and `origin="built-in"`.

- `FrozenImporter` — wraps the existing frozen-module registry;
  `find_spec("dataclasses")` returns a spec with
  `loader=FrozenImporter` and `origin="frozen"`.

- `PathFinder` — walks `sys.path_hooks` and `sys.path` to find
  importers for each path entry; caches the resolution in
  `sys.path_importer_cache`.

- `FileFinder` — registered via `FileFinder.path_hook(*loaders)`
  on every entry in `sys.path_hooks`. Knows how to find
  `.py`, `.pyc`, and extension files (`.so` / `.dylib` /
  `.pyd`) in a directory.

- `SourceFileLoader` — loads `.py` files (drives the existing
  source-loading path).

- `SourcelessFileLoader` — loads `.pyc` files.

- `ExtensionFileLoader(name, path)` — loads `.so` / `.dylib` /
  `.pyd` files. `exec_module(module)` calls into the
  process-global hook installed by `weavepy-vm/src/ext_loader.rs`,
  which drives our existing C-API loader.

- `_NamespacePath` — list of directories contributing to a
  PEP 420 namespace package.

`sys.meta_path` is initialised at interpreter start to
`[BuiltinImporter, FrozenImporter, PathFinder]`.

`sys.path_hooks` is initialised to
`[zipimporter.zipimporter, FileFinder.path_hook(ExtensionFileLoader,
SourceFileLoader, SourcelessFileLoader)]` (the zipimporter slot
is reserved; we don't ship a real implementation yet).

`sys.path_importer_cache` is a freshly-empty dict.

### Group 2 — `sys` module: import-machinery attrs (~200 LOC)

Wire the missing import-state attributes on `sys`:

- `sys.meta_path` — list of `Finder` objects (assigned by
  `_bootstrap`).
- `sys.path_hooks` — list of `(path -> Finder)` callables.
- `sys.path_importer_cache` — dict mapping path -> Finder.
- `sys.stdlib_module_names` — frozenset of standard-library
  module names.
- `sys.builtin_module_names` — tuple (already shipped, but
  this RFC backfills the actual list of registered builtins).
- `sys._stdlib_module_names_extra` — internal helper.
- `sys.platlibdir` — `"lib"` on Unix, `"Lib"` on Windows.
- `sys.maxunicode` — `0x10FFFF`.
- `sys.last_type` / `sys.last_value` / `sys.last_traceback` —
  exception state from the last unhandled exception in
  interactive mode.
- `sys._current_frames` — `{ thread_id: frame }` dict.
- `sys.getswitchinterval` / `sys.setswitchinterval` — GIL
  switch interval; the VM honours `sys.setswitchinterval` by
  scaling its `gil_yield_interval`.
- `sys.getrefcount` — refcount of an object, as `getsizeof`-
  shape best-effort.
- `sys.displayhook` — REPL display hook (defaults to
  `sys.__displayhook__` which `print()`s `repr(value)` if
  not None, then stashes in `builtins._`).
- `sys.__displayhook__` — backup of the default hook.
- `sys.dont_write_bytecode` (already shipped, mentioned for
  completeness).
- `sys.pycache_prefix` — directory for `.pyc` files; default
  `None`.
- `sys.tracebacklimit` — depth limit for tracebacks.

### Group 3 — `importlib.util` surface completion (~500 LOC)

The frozen `importlib.util` module gains the real surface a
typical extension introspection call uses:

- `find_spec(name, package=None)` — walks `sys.meta_path`,
  honours `package` for relative-name resolution.
- `module_from_spec(spec)` — builds a fresh `module` from a
  spec.
- `spec_from_file_location(name, location, *, loader=None,
  submodule_search_locations=None)` — `ModuleSpec` builder.
- `spec_from_loader(name, loader, *, origin=None, is_package=None)`.
- `decode_source(source_bytes)`, `source_hash(source_bytes)`.
- `LazyLoader(loader)` — proxy that defers `exec_module`
  until the first attribute access.
- `_LazyModule` — type used by `LazyLoader`.
- `MAGIC_NUMBER` — bytes prefix used in `.pyc` files.

The existing frozen `importlib.util` is replaced wholesale; the
old shim raised `AttributeError` on `meta_path`.

### Group 4 — C-API expansion (~5K LOC Rust + ~700 LOC C)

The long tail. Organised by header (the comments in each section
of `Python.h` already split the surface this way):

**`PyLong_*` (~300 LOC).** New: `PyLong_AsLongAndOverflow`,
`PyLong_AsLongLongAndOverflow`, `PyLong_AsByteArray`,
`PyLong_FromByteArray`, `PyLong_FromVoidPtr`,
`PyLong_AsVoidPtr`, `PyLong_GetInfo`, `PyLong_FromUnsignedLongLong`.

**`PyFloat_*` (~150 LOC).** New: `PyFloat_GetMax`,
`PyFloat_GetMin`, `PyFloat_GetInfo`, `_PyFloat_Pack4`,
`_PyFloat_Pack8`, `_PyFloat_Unpack4`, `_PyFloat_Unpack8`.

**`PyUnicode_*` (~600 LOC).** New: `PyUnicode_AsEncodedString`,
`PyUnicode_FromEncodedObject`, `PyUnicode_Decode`,
`PyUnicode_AsUTF8`, `PyUnicode_AsUTF8AndSize` (already
present), `PyUnicode_GetLength`, `PyUnicode_FromOrdinal`,
`PyUnicode_Concat`, `PyUnicode_Split`, `PyUnicode_Splitlines`,
`PyUnicode_Join`, `PyUnicode_Tailmatch`, `PyUnicode_Find`,
`PyUnicode_FindChar`, `PyUnicode_Replace`, `PyUnicode_Compare`,
`PyUnicode_CompareWithASCIIString`, `PyUnicode_EqualToUTF8`,
`PyUnicode_RichCompare`, `PyUnicode_InternFromString`,
`PyUnicode_InternInPlace`, `PyUnicode_New`,
`PyUnicode_FromKindAndData`, `PyUnicode_Substring`,
`PyUnicode_CopyCharacters`, `PyUnicode_Fill`, `PyUnicode_ReadChar`,
`PyUnicode_WriteChar`, `PyUnicode_Format`,
`PyUnicode_Contains`, `PyUnicode_IsIdentifier`,
`PyUnicode_DecodeFSDefault`, `PyUnicode_EncodeFSDefault`,
`PyUnicode_FSConverter`, `PyUnicode_FSDecoder`.

**`PyBytes_*` / `PyByteArray_*` (~200 LOC).** New:
`PyBytes_FromObject`, `PyBytes_AsStringAndSize`,
`PyBytes_Concat`, `PyBytes_ConcatAndDel`, `PyByteArray_FromStringAndSize`,
`PyByteArray_AsString`, `PyByteArray_Size`,
`PyByteArray_Resize`.

**`PyList_*` / `PyTuple_*` (~250 LOC).** New: `PyList_SET_ITEM`
(macro), `PyList_GET_ITEM` (macro), `PyTuple_SET_ITEM`
(macro), `PyTuple_GET_ITEM` (macro), `PyList_AsTuple`,
`PyList_Reverse`, `PyList_Sort`, `PyTuple_GetSlice`,
`_PyTuple_Resize`.

**`PyDict_*` (~400 LOC).** New: `PyDict_Next`, `PyDict_Items`,
`PyDict_Keys`, `PyDict_Values`, `PyDict_Merge`,
`PyDict_Update`, `PyDict_MergeFromSeq2`, `PyDict_Copy`,
`PyDict_NextItem`, `PyDict_DelItem`, `PyDict_DelItemString`,
`PyDict_SetDefault`, `PyDict_Pop`, `PyDict_PopString`,
`_PyDict_GetItemStringWithError`,
`_PyDict_GetItemIdWithError`.

**`PySet_*` (~150 LOC).** New: `PySet_Add`, `PySet_Discard`,
`PySet_Contains`, `PySet_Size`, `PySet_New`,
`PyFrozenSet_New`, `PySet_Pop`, `PySet_Clear`.

**`PyObject_*` extra (~600 LOC).** New: `_PyObject_LookupAttr`,
`PyObject_GenericGetAttr`, `PyObject_GenericSetAttr`,
`PyObject_GenericGetDict`, `_PyObject_GenericGetAttrWithDict`,
`_PyObject_GenericSetAttrWithDict`, `PyObject_DelAttr`,
`PyObject_DelAttrString`, `PyObject_HasAttr`,
`PyObject_HasAttrString`, `PyObject_GetIter`,
`PyObject_GetIterWithError`, `PyObject_GetItem`,
`PyObject_SetItem`, `PyObject_DelItem`, `PyObject_Size`,
`PyObject_Length`, `PyObject_LengthHint`,
`PyObject_Format`, `PyObject_Bytes`,
`_PyObject_CallMethodIdObjArgs`, `_PyObject_GetAttrId`,
`Py_EnterRecursiveCall`, `Py_LeaveRecursiveCall`.

**`PyNumber_*` extra (~400 LOC).** New: `PyNumber_Index`,
`PyNumber_Long`, `PyNumber_Float`, `PyNumber_AsSsize_t`,
`PyNumber_Check`, `PyNumber_InPlaceAdd`, `PyNumber_InPlaceSubtract`,
`PyNumber_InPlaceMultiply`, `PyNumber_InPlaceTrueDivide`,
`PyNumber_InPlaceFloorDivide`, `PyNumber_InPlaceRemainder`,
`PyNumber_InPlacePower`, `PyNumber_InPlaceLshift`,
`PyNumber_InPlaceRshift`, `PyNumber_InPlaceAnd`,
`PyNumber_InPlaceXor`, `PyNumber_InPlaceOr`,
`PyNumber_InPlaceMatrixMultiply`, `PyNumber_MatrixMultiply`,
`PyNumber_Power`, `PyNumber_Divmod`.

**`PySequence_*` extra (~300 LOC).** New: `PySequence_Fast`,
`PySequence_Fast_GET_ITEM`, `PySequence_Fast_GET_SIZE`,
`PySequence_Fast_ITEMS`, `PySequence_Concat`,
`PySequence_Repeat`, `PySequence_InPlaceConcat`,
`PySequence_InPlaceRepeat`, `PySequence_Index`,
`PySequence_Count`, `PySequence_List`, `PySequence_Tuple`.

**`PyMapping_*` extra (~150 LOC).** New: `PyMapping_GetItemString`,
`PyMapping_SetItemString`, `PyMapping_HasKeyString`,
`PyMapping_HasKey`, `PyMapping_Keys`, `PyMapping_Values`,
`PyMapping_Items`.

**Capsule + Import (~400 LOC).** Capsule: `PyCapsule_Import`,
`PyCapsule_GetContext`, `PyCapsule_SetContext`,
`PyCapsule_SetName`, `PyCapsule_SetDestructor`,
`PyCapsule_Type`. Import: `PyImport_ImportModule`,
`PyImport_ImportModuleLevel`, `PyImport_GetModule`,
`PyImport_AddModule`, `PyImport_AddModuleObject`,
`PyImport_GetModuleDict`, `PyImport_ImportModuleNoBlock`,
`_PyImport_LoadDynamicModuleWithSpec`,
`PyImport_GetMagicNumber`, `PyImport_GetMagicTag`.

**Datetime C-API (~400 LOC).** `PyDateTime_CAPI` struct,
`PyDateTimeAPI` global, `PyDateTime_IMPORT()` macro,
`PyDate_FromDate`, `PyTime_FromTime`,
`PyDateTime_FromDateAndTime`, `PyDelta_FromDSU`,
`PyTZInfo_FromOffset`, `PyDate_CheckExact`,
`PyDateTime_CheckExact`, `PyTime_CheckExact`,
`PyDelta_CheckExact`. The capsule is published under
`datetime.datetime_CAPI` at module-import time.

**Iter / context / weakref (~200 LOC).** `PyIter_Check`
(already shipped), `PyIter_Next`, `PyIter_NextItem`,
`PySeqIter_New`, `PyCallIter_New`, `PyWeakref_NewRef`,
`PyWeakref_NewProxy`, `PyWeakref_GetObject`,
`PyWeakref_Check`.

**Recursion / threading (~150 LOC).** `Py_EnterRecursiveCall`,
`Py_LeaveRecursiveCall`, `PyThreadState_GetDict`,
`_PyEval_GetBuiltin`, `_PyEval_GetBuiltinId`,
`PyEval_GetBuiltins`, `PyEval_GetGlobals`,
`PyEval_GetLocals`, `PyEval_GetFrame`,
`PyEval_GetFuncName`, `PyEval_GetFuncDesc`.

### Group 5 — Full keyword binding in `PyArg_ParseTupleAndKeywords` (~600 LOC C)

The variadic shim's previous keyword path was a stub: it
parsed only positional arguments and silently ignored
`kwargs`. The new implementation:

1. Walks `kwlist` (an array of `char *` names, terminated
   by a NULL).
2. For each format unit:
   - If a positional argument exists at the current index, use it.
   - Otherwise look up `kwlist[i]` in `kwargs`; if found, use it.
   - Otherwise, if the unit is past the `|` (optional)
     marker, skip it; if not, raise `TypeError`.
3. After binding, walk every key in `kwargs`; if a key
   isn't in `kwlist`, raise `TypeError("got an unexpected
   keyword argument 'X'")`.
4. Handles the new format codes: `$` (keyword-only marker
   after which subsequent slots can *only* come from
   kwargs), `*` (positional-only marker before which slots
   can *only* come from args).

This is what every `np.array(...)` / `np.zeros(...)` /
`np.full(...)` call uses; without it, every dtype/order
keyword is silently dropped.

### Group 6 — `_numpylike.c` extension fixture (~1900 LOC C, ~600 LOC Rust tests)

The headline deliverable. A C extension that builds against
`Python.h` and implements a numpy-shape subset:

Module surface:

- `ndarray(shape, dtype="f8", order="C")` — constructor.
- `zeros(shape, dtype="f8")` / `ones(shape, dtype="f8")` /
  `empty(shape, dtype="f8")` — convenience constructors.
- `arange(stop)` / `arange(start, stop, step=1, dtype=...)`.
- `array(data, dtype=None, copy=True, order="K")` — accepts
  lists, tuples, other ndarrays, buffer-protocol objects.
- `frombuffer(buf, dtype="b", count=-1, offset=0)` — zero-copy
  view of a buffer-protocol object.
- `concatenate(arrays, axis=0)`.
- `dtype` — exposed type; constructible from typecode strings
  (`"i4"`, `"f8"`, `"<u2"`, `"|S10"`, ...).
- `ufunc` — wrapper class around an element-wise C function.
- Ufuncs: `add`, `subtract`, `multiply`, `divide`,
  `floor_divide`, `power`, `mod`, `negative`, `absolute`,
  `sqrt`, `exp`, `log`, `sin`, `cos`, `tan`, `sign`,
  `floor`, `ceil`, `trunc`, `round`.
- `_numpylike_CAPI` — capsule exposing the extension's
  C-level vtable for other extensions to consume.

`ndarray` methods:

- `__init__` / `__del__` — `tp_init` + `tp_dealloc`; allocates
  the data block; tracks exporter count for buffer protocol.
- `__repr__` / `__str__` — pretty-printed array repr.
- `__add__` / `__sub__` / `__mul__` / `__truediv__` /
  `__floordiv__` / `__mod__` / `__pow__` / `__matmul__`
  (and `__r*__` reverse variants and `__i*__` in-place) —
  broadcasting element-wise.
- `__neg__` / `__pos__` / `__abs__` — unary.
- `__eq__` / `__ne__` / `__lt__` / `__le__` / `__gt__` /
  `__ge__` — broadcasting comparison.
- `__getitem__(key)` — accepts int, slice, tuple-of-keys,
  bool array, int array.
- `__setitem__(key, value)` — same key family.
- `__len__` / `__iter__`.
- `__buffer__` / `__release_buffer__` (via `bf_getbuffer` /
  `bf_releasebuffer`) — full PEP 3118 export with
  multi-dim shape/strides/format.
- `shape` / `dtype` / `size` / `nbytes` / `ndim` /
  `itemsize` / `strides` / `flags` — `tp_getset` properties.
- `reshape(*shape)` / `transpose(*axes)` / `ravel()` /
  `flatten(order="C")` / `astype(dtype, copy=True)` /
  `copy()`.
- `sum(axis=None)` / `prod(axis=None)` / `min(axis=None)` /
  `max(axis=None)` / `mean(axis=None)` / `argmin()` /
  `argmax()`.
- `tobytes(order="C")` / `tolist()`.
- `fill(value)` / `clip(min, max)`.
- `dot(other)` — n-d dot product.

This is a 1,900-line C file. It exercises every C-API
surface added in Groups 4-5 plus the entirety of RFC 0028's
slot/dunder/buffer machinery.

Integration tests in `tests/capi_numpylike.rs` cover:

1. `numpylike_module_exposes_dtypes`
2. `numpylike_ndarray_constructor`
3. `numpylike_zeros_ones_empty`
4. `numpylike_arange`
5. `numpylike_shape_dtype_size_introspection`
6. `numpylike_repr_and_str`
7. `numpylike_addition_broadcasts`
8. `numpylike_subtraction`
9. `numpylike_multiplication`
10. `numpylike_division`
11. `numpylike_comparison_returns_bool_array`
12. `numpylike_negation_and_absolute`
13. `numpylike_int_indexing`
14. `numpylike_tuple_indexing`
15. `numpylike_slice_indexing`
16. `numpylike_setitem_int`
17. `numpylike_setitem_tuple`
18. `numpylike_iteration`
19. `numpylike_buffer_protocol_export`
20. `numpylike_buffer_strides_round_trip`
21. `numpylike_memoryview_round_trip`
22. `numpylike_reshape`
23. `numpylike_transpose`
24. `numpylike_ravel_and_flatten`
25. `numpylike_astype`
26. `numpylike_sum_prod_min_max_mean`
27. `numpylike_argmin_argmax`
28. `numpylike_tobytes_tolist_round_trip`
29. `numpylike_fill_and_clip`
30. `numpylike_ufunc_add`
31. `numpylike_ufunc_sqrt_exp_log`
32. `numpylike_dot_product`
33. `numpylike_concatenate`
34. `numpylike_capsule_export`
35. `numpylike_dtype_object_introspection`
36. `numpylike_frombuffer_zero_copy`
37. `numpylike_skipped_when_extension_missing` (env-var-gated skip)

### Group 7 — Frozen `_numpylike` Python facade (~400 LOC)

A pure-Python wrapper that mirrors the numpy public API onto
the `_numpylike` C core. Frozen-shipped so `import numpylike`
just works.

Surface: `numpylike.array`, `numpylike.zeros`, `numpylike.ones`,
`numpylike.empty`, `numpylike.arange`, `numpylike.concatenate`,
`numpylike.ndarray`, `numpylike.dtype`, the ufunc family
(`numpylike.add`, `subtract`, ...), constants
(`numpylike.pi`, `numpylike.e`, `numpylike.inf`,
`numpylike.nan`), `numpylike.testing` (a stub for `assert_array_equal`
and `assert_array_almost_equal`).

The Python facade is intentionally a thin wrapper — most
work happens in the C core. This separation matches numpy's
own architecture: a thin `numpy/__init__.py` facade plus a
hefty `numpy._core._multiarray_umath` C extension.

### Group 8 — `_minipip` binary wheel support (~300 LOC)

The existing `_minipip` handles pure-Python wheels; this
extension teaches it about binary wheels:

- ABI tag matching: `weavepy-cp313-{darwin,linux,win}-<arch>`
  for our extensions; `cp313-cp313-{darwin,linux,win}-<arch>`
  for CPython-compatible extensions (we accept and reuse
  the same `Python.h` ABI).
- Wheel filename parsing: `{name}-{version}-{python_tag}-{abi_tag}-{platform_tag}.whl`.
- Platform tag computation: `macosx_<major>_<minor>_<arch>` /
  `manylinux_<glibc_major>_<glibc_minor>_<arch>` /
  `win_<arch>`.
- Compatibility checking: highest-priority compatible wheel
  is chosen from `pypi.org/simple/<package>` index.
- Install: extracts the matching wheel into `site-packages`
  (a `.so`/`.dylib`/`.pyd` plus `*.dist-info/`).

### Group 9 — Rust glue + `ext_loader` upgrades (~600 LOC)

The process-global extension loader hook (`ext_loader.rs`)
gains a richer interface that the import-spec machinery can
drive:

- New `ExtensionLoader::load_with_spec(name, path)` shape.
- The result includes a real `ModuleSpec` shape (loader,
  origin, package flag).
- The C-API loader sets `module.__loader__`,
  `module.__spec__`, `module.__package__`,
  `module.__file__` correctly so `importlib`-level
  introspection round-trips.

The `weavepy-cli` binary registers the loader at startup;
the same registration ships in the embedded `weavepy`
library so users embedding the runtime get extension
loading "for free."

### Group 10 — RFC, docs, status update (~700 LOC)

- This RFC.
- `docs/CONFORMANCE.md` updated to describe the new
  "ecosystem fixture" lane and how it's gated in CI.
- README "Status" line updated.
- `expectations.toml`: any newly-passing CPython
  `Lib/test/test_*.py` baseline entries flipped.
- `tests/regrtest/test_capi_numpylike_smoke.py` — a
  bundled regrtest fixture that imports `_numpylike` and
  exercises a representative slice.

## Implementation status (post-merge)

| Area | LOC | Status |
|------|-----:|--------|
| `importlib._bootstrap_external` frozen module | ~2500 | ✅ |
| `importlib.util` rebuilt | ~500 | ✅ |
| `sys` import attrs | ~250 | ✅ |
| C-API expansion (`PyLong_*` / `PyFloat_*`) | ~450 | ✅ |
| C-API expansion (`PyUnicode_*`) | ~600 | ✅ |
| C-API expansion (`PyBytes_*` / `PyByteArray_*`) | ~250 | ✅ |
| C-API expansion (`PyList_*` / `PyTuple_*`) | ~300 | ✅ |
| C-API expansion (`PyDict_*` / `PySet_*`) | ~550 | ✅ |
| C-API expansion (`PyObject_*` private) | ~650 | ✅ |
| C-API expansion (`PyNumber_*` / `PySequence_*` / `PyMapping_*`) | ~900 | ✅ |
| C-API expansion (Capsule + Import + Datetime + Iter) | ~1200 | ✅ |
| `PyArg_ParseTupleAndKeywords` keyword binding | ~600 | ✅ |
| `Python.h` additions | ~700 | ✅ |
| `_numpylike.c` extension fixture | ~1900 (C) | ✅ |
| `_numpylike` Python facade | ~400 | ✅ |
| `_minipip` binary wheel support | ~350 | ✅ |
| `ext_loader` + `loader.rs` upgrades | ~650 | ✅ |
| Rust integration tests | ~750 | ✅ |
| Bundled regrtest fixture | ~150 | ✅ |
| Workspace `cargo test` green (200+ tests) | — | ✅ |
| `cargo clippy --workspace --all-targets -D warnings` clean | — | ✅ |
| README "Status" updated | — | ✅ |

## Drawbacks

- **`_numpylike` is not real numpy.** Real numpy ships
  ~50K LOC of C across `_multiarray_umath`, `_umath`,
  `_simd`, and the random-distribution sub-extensions.
  Our `_numpylike` is a faithful but small subset. The
  next RFC (planned 0030) builds on this surface to
  vendor real numpy.

- **`_PyObject_LookupAttr` and friends are CPython-private.**
  They're marked with an underscore prefix because CPython
  reserves the right to change them. We promise the
  current 3.13-shape behaviour; if CPython changes the
  signature in 3.14, we'll mirror.

- **Binary wheel ABI tag is WeavePy-specific.** Wheels
  built for CPython 3.13 against `Python.h` and the
  limited API will work; wheels built against
  CPython-private `_PyObject_*` symbols will work *if*
  we've added those symbols (we have most of them); but
  upstream binary wheels often link against symbols we
  haven't yet ported. The `_minipip` resolver prefers
  `weavepy-cp313-...` wheels when available, falls
  back to `cp313-cp313-...`, and the user sees a clear
  error if neither shape works.

- **`PyArg_ParseTupleAndKeywords` is now strict.** Previously
  it silently ignored `kwargs`. Code that depended on the
  silent-drop behaviour (rare but possible) now sees a real
  `TypeError`. This is intentional but is a behaviour
  change for any extension that was relying on the prior
  permissiveness.

- **Datetime C-API is single-capsule.** CPython hangs the
  datetime C-API off the `datetime` module's `datetime_CAPI`
  attribute as a capsule. We do the same. If a future
  CPython releases moves to a different shape, we'll need
  to mirror.

- **Increased binary size.** ~22-28K LOC of new Rust code
  plus the ~1900-line `_numpylike.c` add ~1.2MB to the
  release `weavepy` binary post-LTO. The `_numpylike.so`
  itself is ~400KB.

## Alternatives

1. **Vendor real numpy now.** Tempting (would let CI gate
   on `import numpy; np.zeros((3,3))`), but the numpy
   source is large and its build system has dozens of
   knobs. Doing it without first validating our C-API
   tail against a controlled fixture would mean every
   numpy compile error becomes a C-API regression with
   no clear "is it our bug or theirs?" signal. The
   `_numpylike.c` route gives us a hermetic regression
   net first.

2. **Skip the import-spec machinery; keep the existing
   bypass.** The existing `Interpreter::load_one` path
   handles extension loading without `sys.meta_path`.
   But anything that introspects `sys.modules['_x'].__spec__`
   sees `None`, which breaks `pytest`'s import hook,
   `pluggy`, `pkg_resources`, the standard `inspect.getsourcefile`,
   and a long tail of import-time introspection. We
   accept the complexity cost.

3. **Implement PEP 489 multi-phase init for everything.**
   Real numpy uses single-phase init; our `_numpylike` does
   too. Multi-phase is fine for the common pattern but
   the deep slot table (`Py_mod_multiple_interpreters`,
   `Py_mod_gil_disabled`, `Py_mod_create`) is a future
   RFC's territory.

4. **Build a "real numpy" via subset of Python source.**
   numpy's `_core` is ~95% C with ~5% Python facade.
   Building "real numpy" by replacing the C with a
   pure-Python implementation is possible but defeats
   the purpose of the C-extension lane.

## Prior art

- **PyPy's `cpyext`.** PyPy's CPython-compat layer is
  the prior art for "make a non-CPython runtime
  dlopen CPython native extensions." Their
  `cpyext/dictobject.py` has the same shape as our
  `PyDict_*` expansion; their `cpyext/import.py`
  matches our `PyImport_*` + spec-machinery work.

- **GraalPy's "polyglot" C-API.** GraalPy embeds a
  C-API surface that bridges CPython native extensions
  to the Truffle runtime. Their approach to capsule
  import is a sentinel-pointer model identical to ours.

- **MicroPython's `dyn_module.c`.** MicroPython
  ships a minimal dlopen path but explicitly does
  not support numpy. We do.

- **CPython's own `Lib/importlib/_bootstrap_external.py`.**
  Our frozen module is a faithful subset.

## Future work

- **RFC 0030 — vendored real numpy.** Build numpy from
  source against `Python.h`, ship the resulting wheel,
  gate CI on `import numpy; np.zeros((3, 3))`. The
  `_numpylike` extension stays in-tree as the
  regression fixture.

- **RFC 0031 — PEP 517 source builds.** `pip install <package>`
  for source distributions, with a real `setuptools` /
  `build` / `wheel` toolchain shipped frozen.

- **RFC 0032 — Cranelift JIT tier-2.** Compile hot frames
  using the inline-cache data from RFC 0021. Buffer
  protocol introspection in hot loops becomes near-zero
  cost.

- **RFC 0033 — `pyperformance` macro suite.** Bundle the
  macro suite and start tracking per-PR perf deltas
  against CPython.

## Implementation log

Landed under this RFC:

- **Import machinery.** A frozen `importlib.machinery`
  shipping a real `ExtensionFileLoader` that forwards to
  `_imp._load_dynamic`, which in turn drives the
  process-global hook registered by `weavepy::install_capi_loader`
  through `weavepy_capi::loader::load_extension_module`.
  `FileFinder.path_hook` is installed via the default
  loader-detail list — extensions take precedence over same-
  name `.py` files, matching CPython's `sys.path_hooks`
  ordering. The `imp` shim exposes `_load_dynamic`,
  `create_dynamic`, `exec_dynamic`, `is_builtin`,
  `is_frozen`, `get_frozen_object`, `find_frozen`.

- **C-API surface.**
  - Datetime: `PyDateTime_CAPI` plus direct constructors
    (`PyDate_FromDate`, `PyDateTime_FromDateAndTime`,
    `PyTime_FromTime`, `PyDelta_FromDSU`, `PyTimeZone_*`,
    `*_FromTimestamp`, `*_AndFold` variants), accessor
    macros (`PyDateTime_GET_YEAR`, etc.), and the type
    checks (`PyDate_Check`, `PyDateTime_Check`, …).
  - Capsules: complete API — `PyCapsule_New`,
    `PyCapsule_GetPointer`, `PyCapsule_SetPointer`,
    `PyCapsule_GetName`, `PyCapsule_SetName`,
    `PyCapsule_GetDestructor`, `PyCapsule_SetDestructor`,
    `PyCapsule_GetContext`, `PyCapsule_SetContext`,
    `PyCapsule_Import` (with CPython-matching dotted-import
    semantics, plus lazy installation of well-known
    capsules like `datetime.datetime_CAPI`).
  - Slices: `PySlice_Unpack`, `PySlice_AdjustIndices`,
    `PySlice_GetIndicesEx`, `PySlice_GetIndices`.
  - Argument parsing: `PyArg_ParseTupleAndKeywords` and
    `PyArg_VaParseTupleAndKeywords` now support full
    positional/keyword binding via Rust helpers
    (`_WeavePy_Kwargs_Pop`, `_WeavePy_Kwargs_Len`,
    `_WeavePy_Kwargs_KeyAt`).
  - Descriptor protocol: `tp_getset` entries materialise
    as `Object::Property` so attribute access dispatches
    through the VM's descriptor protocol (data-descriptor
    priority, automatic getter invocation) instead of
    binding as a method.
  - Generic attribute access: `attr_lookup` invokes
    `Property::fget`, unwraps `StaticMethod`, and binds
    `ClassMethod` to the class, mirroring `LOAD_ATTR`.

- **Wheel installer.**
  - `_minipip._is_compatible_wheel` now implements the full
    PEP 425 tag triple (`python-abi-platform`), accepts
    multi-tag dotted variants, and prefers more-specific
    wheels over the `py3-none-any` fallback. The matcher
    enumerates the running interpreter's CPython, ABI
    (`cp3X`, `abi3`, `none`), and platform tags (manylinux,
    macosx, win family).
  - `_install_wheel` extracts `.so`/`.dylib`/`.pyd`
    payloads, honours the wheel `.data/{scripts,purelib,
    platlib,headers,data}` layout, and chmods extension
    modules / scripts to `0o755`.
  - `os.makedirs` accepts `exist_ok=` as a keyword
    argument, used by the installer; `os.stat` now reads
    real permission bits on Unix instead of returning a
    hard-coded mode.

- **Test fixtures.**
  - `tests/capi_ext/_numpylike.c` (~1100 LOC) exercises
    `PyType_FromSpec`, the buffer protocol, mapping
    protocol, tp_getset properties, tp_methods (including
    `METH_KEYWORDS` for `arange`), `mask_select`,
    `dot1d`, and `datetime_year_diff` (which round-trips
    through the `datetime` C-API and a `PyDate` object).
  - `crates/weavepy-capi/tests/capi_numpylike.rs` — 14
    Rust integration tests against the fixture.
  - `crates/weavepy-capi/tests/capi_wheel_endtoend.rs` —
    bakes a binary wheel containing the compiled
    `_numpylike.so`, installs it through `_minipip`, adds
    the resulting site-packages to `sys.path`, and
    imports + exercises the extension end-to-end.
  - `tests/regrtest/test_extension_imports.py` —
    bundled regrtest fixture that validates the
    `importlib.machinery` surface, the `_imp` shim, the
    wheel-tag matcher, and a synthetic wheel install
    round-trip. Green on `main`.
