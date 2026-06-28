# RFC 0043: CPython 3.13 binary-ABI compatibility (cpyext) - wave 1: the layout-faithful object bridge and stock-extension loading

- **Status**: Accepted
- **Authors**: WeavePy authors
- **Created**: 2026-06-27
- **Tracking issue**: TBD
- **Builds on**: RFC 0022 (the C-API foundation - `Python.h`, the `dlopen`
  loader, the `PyObject` handle bridge), RFC 0028 (PEP 3118 buffer protocol,
  PEP 590 vectorcall, the `PyType_FromSpec[WithBases]` slot surface), RFC 0029
  (the numpy-grade end-to-end path - PEP 451 import machinery,
  `ExtensionFileLoader`, the ~120-symbol C-API tail, `_minipip` binary-wheel
  install, the in-tree `_numpylike.c` fixture).

## Summary

Today WeavePy can `dlopen` and run C extensions, but only ones **recompiled
against WeavePy's own `Python.h`**. That header (RFC 0022) deliberately tracks
CPython's `Py_LIMITED_API` *shape* and routes every value access - even
`Py_INCREF` - through a *function call* into the host, because a WeavePy object
is a Rust `Object` enum (`Float(f64)`, `Long(Rc<BigInt>)`, `Str(Rc<str>)`,
`Tuple(Rc<[Object]>)`, ...) with **none of CPython's struct fields at the
offsets a stock header poke**.

A *stock* PyPI wheel (`numpy-*-cp313-cp313-macosx_11_0_arm64.whl`) is the
opposite: it was compiled against CPython's real headers, so it contains
**inlined macros that read struct fields at fixed offsets** -
`Py_INCREF`/`Py_DECREF` poking `ob_refcnt`, `Py_TYPE`/`Py_SIZE`,
`PyFloat_AS_DOUBLE`, `PyList_GET_ITEM`, `PyTuple_GET_ITEM`,
`PyBytes_AS_STRING`, the PEP 393 compact-string `PyUnicode_DATA`/`KIND`, and
direct reads of `PyTypeObject` slots. Those macros read WeavePy's Rust payload
as if it were a CPython struct and get garbage.

This RFC opens the **binary-ABI** (cpyext-style) effort: make WeavePy's host
binary export a C-API that is **byte-for-byte and behaviourally faithful to
CPython 3.13's full (non-limited) ABI**, so that *unmodified* stock extensions
load and run. Because a single process exports exactly one set of `Py*`
symbols, this is necessarily a **conversion** of WeavePy's exported surface, not
an additive mode - and it is large enough to be sequenced across several waves.

**Wave 1 (this commit)** lands the foundation and proves the thesis
hermetically:

1. **Faithful concrete-object layouts.** Byte-exact CPython 3.13 structs for the
   high-frequency types whose internals get inlined: `PyVarObject`,
   `PyFloatObject`, `PyLongObject` (the 3.12+ `_PyLongValue` `lv_tag` + 30-bit
   `ob_digit` form), `PyBytesObject`, `PyTupleObject`, `PyListObject`, and the
   PEP 393 `PyASCIIObject`/`PyCompactUnicodeObject`/`PyUnicodeObject` compact
   string forms - plus a full-layout `PyTypeObject`/`PyHeapTypeObject` and the
   method-suite structs.
2. **The object mirror bridge.** A cpyext-style identity map between a native
   `Object` and a heap-allocated **mirror** whose memory is laid out exactly
   like the corresponding CPython struct. When a value crosses into C it is
   mirrored (and cached); when C hands a pointer back, the mirror is resolved to
   its native `Object`. Mutations sync across the boundary. The bridge owns the
   refcount<->lifetime contract.
3. **Refcount/immortality fidelity.** The immortal-refcount sentinel switches to
   CPython 3.13's "low 32 bits set" form (`_Py_IMMORTAL_REFCNT`), so a stock
   *inlined* `Py_INCREF`/`Py_DECREF` (which special-cases immortals by testing
   the low half-word) treats WeavePy singletons/static types correctly.
4. **Stock-header symbol surface.** The high-frequency core of the full C-API
   (module init, arg parsing, the `PyFloat_*`/`PyLong_*`/`PyList_*`/`PyTuple_*`/
   `PyBytes_*`/`PyUnicode_*` constructors and accessors) exported with
   CPython-faithful signatures and semantics over the mirror bridge.
5. **A hermetic proof.** A C extension compiled against the host's **stock
   CPython 3.13 headers** (full API, real inlined macros, a static
   `PyModuleDef`/`PyMethodDef`, returning and consuming the core types) is
   `dlopen`ed into WeavePy and exercised end-to-end - the first time a stock
   ABI artifact (rather than a WeavePy-header artifact) runs under WeavePy.
6. **Loader/installer recognition.** The extension loader and `_minipip` accept
   the stock `cp313-cp313`/`abi3` wheel tags (not just `weavepy-cp313`), so a
   stock binary wheel resolves to the matching `.so` and is handed to the
   faithful loader.

What wave 1 does **not** do: import real numpy. numpy's `_multiarray_umath`
links a large, partly-private slice of the full C-API plus the array-object
C-API capsule; that surface is sequenced into waves 2-5 (see Roadmap). Wave 1's
acceptance bar is "a stock-compiled extension that uses the core object/type/
module surface loads and runs, proven by an in-tree fixture and a bundled
regrtest", with the whole workspace `build`/`fmt`/`clippy`/regrtest green.

## Motivation

The README's promise is to "run existing Python code, packages, tools, and
workflows unchanged." For *pure-Python* packages WeavePy already delivers
(RFC 0030's `pip`/`numpy` facade/`pytest`). For *native* packages the story
stops at "recompile against WeavePy's `Python.h`" (RFC 0022/0028/0029). But the
binary wheels users actually `pip install` - numpy, pandas, pillow, lxml,
cryptography, pydantic-core - are shipped **pre-compiled against stock CPython**.
Loading them unchanged is the single highest-ceiling drop-in lever (named as
such in RFC 0041's Future work), and it is the one capability gap that no
amount of frozen-Python porting can close.

The reason it is hard - and why it is its own multi-wave arc - is the
**representation gap**. CPython's extensions are not merely *callers* of an API;
they are *readers of memory*. A decade of CPython headers inline the hot path:

```c
/* stock CPython 3.13, floatobject.h */
static inline double PyFloat_AS_DOUBLE(PyObject *op) {
    return _PyFloat_CAST(op)->ob_fval;        /* reads *(double*)(op+16) */
}
/* stock CPython 3.13, listobject.h */
#define PyList_GET_ITEM(op, i)  (_PyList_CAST(op)->ob_item[i])
/* stock CPython 3.13, object.h */
static inline void Py_INCREF(PyObject *op) {
    if (_Py_IsImmortal(op)) return;           /* tests op->ob_refcnt low half */
    op->ob_refcnt++;
}
```

WeavePy's value for a float is `Object::Float(f64)` inside a `PayloadCell` that
begins *after* the 16-byte object head - so `*(double*)(op+16)` reads the first
8 bytes of a Rust enum, not the IEEE-754 double. There is no way to satisfy
these inlined readers except to **make the memory at those offsets be what
CPython says it is**. That is precisely what PyPy's `cpyext` and GraalPy's
C-API layer do: they maintain a parallel, layout-faithful "mirror" of each
object that has crossed into C, and keep it coherent with the runtime's native
object.

This RFC commits WeavePy to that model and lands its load-bearing core.

## The central problem, precisely

Three sub-problems, each of which wave 1 must address for the core types:

1. **Field layout.** For every concrete type whose header inlines field access,
   the mirror's bytes must match CPython 3.13 exactly: offsets, sizes,
   bit-field packing (the PEP 393 `state` word), and the variable-length tail
   (`ob_item[]`, `ob_digit[]`, `ob_sval[]`, the inline character buffer).

2. **Coherence + lifetime.** A mirror and its native `Object` must stay in sync
   for as long as C holds a reference. Immutable scalars (float/int/bytes/str)
   are filled once at mirror time. Mutable containers (list) need write-through
   on `PyList_SET_ITEM`/`PyList_Append`. The C-side `ob_refcnt` governs when the
   mirror (and the native reference it pins) may be released.

3. **Identity.** `x is y` in Python must remain true after a round trip through
   C, and `Py_TYPE(op)` must return the *same* `PyTypeObject*` the extension
   compares against (`Py_TYPE(op) == &PyFloat_Type`). The bridge therefore keys
   mirrors by native identity where identity is observable, and exports the
   built-in type objects as faithful statics.

## CPython reference

Wave 1 matches **CPython 3.13** as installed on the build host
(`python3.13`, 3.13.x) and cross-checked against the vendored
`vendor/cpython/` tree. The authoritative layouts:

- `Include/object.h` - `PyObject`, `PyVarObject`, `_Py_IMMORTAL_REFCNT`
  (`UINT_MAX` on 64-bit), the `Py_INCREF`/`Py_DECREF`/`Py_SIZE`/`Py_TYPE`
  inline forms.
- `Include/cpython/object.h` - the full `PyTypeObject` field order through
  `tp_versions_used`, and `PyHeapTypeObject` (method suites + `ht_name`/
  `ht_qualname`/`ht_module`/`_ht_tpname`/`_spec_cache`).
- `Include/floatobject.h` - `PyFloatObject { PyObject_HEAD; double ob_fval; }`.
- `Include/cpython/longintrepr.h` - `_PyLongValue { uintptr_t lv_tag; digit
  ob_digit[1]; }` with 30-bit digits; `lv_tag` packs `ndigits << 3` with sign
  in the low 2 bits (0 positive, 1 zero, 2 negative).
- `Include/cpython/tupleobject.h` / `Include/cpython/listobject.h` -
  `PyTupleObject { PyObject_VAR_HEAD; PyObject *ob_item[1]; }`,
  `PyListObject { PyObject_VAR_HEAD; PyObject **ob_item; Py_ssize_t allocated; }`.
- `Include/cpython/bytesobject.h` - `PyBytesObject { PyObject_VAR_HEAD;
  Py_hash_t ob_shash; char ob_sval[1]; }`.
- `Include/cpython/unicodeobject.h` - the PEP 393 `PyASCIIObject` /
  `PyCompactUnicodeObject` / `PyUnicodeObject` forms and the `state` bit-field
  (`interned:2, kind:3, compact:1, ascii:1, statically_allocated:1`).
- `Include/methodobject.h`, `Include/moduleobject.h` - `PyMethodDef`,
  `PyModuleDef`/`PyModuleDef_Base` (already faithful in WeavePy's header).
- PEP 3123 (standard layout for `PyObject`), PEP 393 (flexible string repr),
  PEP 683 (immortal objects).

Explicit non-references (out of scope for the binary ABI, here and later):
PEP 703 free-threading (`Py_GIL_DISABLED` layouts), the `Py_TRACE_REFS` debug
head, and Windows `.pyd` loading.

## Current baseline (measured starting point)

- `cargo build --workspace` is green.
- Bundled `tests/regrtest/` suite is `--check` clean; the CPython `Lib/test/`
  allowlist sweep stands at the RFC 0042 numbers (180 pass / 32 fail / 13 skip /
  2 timeout over 227 tracked, 1 known pre-existing flake).
- `weavepy-capi` exports the `Py_LIMITED_API`-shaped surface: `PyObject`/
  `PyVarObject` heads are faithful, but `PyTypeObject` is a *subset* (head then
  `tp_name`/`tp_basicsize`/`tp_itemsize`/`tp_flags`/`tp_slots`/`bridge`), the
  concrete object structs are **not** exposed (payload is a Rust `Object`), and
  every accessor macro is a function call.
- Extensions are built against WeavePy's `include/Python.h`; the proof fixtures
  (`_smalltest.c`, `_ndarray.c`, `_numpylike.c`) all use that header. **No
  artifact compiled against stock CPython headers has ever been loaded.**
- `IMMORTAL_REFCNT = (isize::MAX/2) - 1` - whose low 32 bits are `0xFFFF_FFFE`,
  *not* CPython's `0xFFFF_FFFF`, so a stock inlined immortality check would
  misclassify WeavePy statics.

## Roadmap (the multi-wave arc)

D1 is large; it is sequenced so each wave is independently green and
fixture-proven:

- **Wave 1 (this RFC).** Faithful core object/type/module layouts; the mirror
  bridge for scalars + the core containers; immortality fidelity; the
  high-frequency symbol core; a stock-headers proof extension; loader/installer
  tag recognition.
- **Wave 2.** The full type-suite round trip: `PyNumberMethods`/
  `PySequenceMethods`/`PyMappingMethods`/`PyAsyncMethods`/`PyBufferProcs` read
  from stock static types and heap types; descriptor (`tp_descr_get/set`),
  `tp_getattro`/`tp_setattro`, `tp_call`, `tp_iter`/`tp_iternext`,
  `tp_richcompare` dispatch from the faithful slots; GC integration
  (`tp_traverse`/`tp_clear` participating in WeavePy's cycle collector).
- **Wave 3.** The numpy *C-API surface*: the `PyArray_*` import capsule shape,
  `__array_struct__`/`__array_interface__`, the ufunc registration path, plus
  the long tail of `_multiarray_umath`'s symbol dependencies (the "is it our
  bug or theirs" hermetic fixtures expand here).
- **Wave 4.** Build **real numpy** from source against the now-faithful host
  ABI; gate CI on `import numpy; numpy.zeros((3,3)) @ numpy.ones((3,3))`.
- **Wave 5.** pandas / Cython-generated extensions (the full-API,
  heavily-macro'd Cython surface) and the manylinux/macos wheel matrix.

## Detailed design (wave 1)

Six workstreams, in dependency order. Line-count estimates include Rust, the C
fixture, the faithful header work, and tests.

### WS1 - Faithful layouts + a CPython-faithful header path (~3K LOC)

A new `layout` module in `weavepy-capi` defines `#[repr(C)]` Rust structs that
are byte-identical to CPython 3.13's, with compile-time
`const _: () = assert!(size_of/offset_of ...)` guards pinned to the values read
out of the host's stock headers (so a CPython point-release layout drift fails
the build loudly rather than silently corrupting memory):

- `PyVarObject` (head + `ob_size`), and the immortal sentinel constant moved to
  the CPython form.
- `PyFloatObject`, `PyLongObject` + `_PyLongValue`, `PyComplexObject`,
  `PyBytesObject`, `PyByteArrayObject`, `PyTupleObject`, `PyListObject`,
  `PyASCIIObject`/`PyCompactUnicodeObject`/`PyUnicodeObject`.
- The full `PyTypeObject` (all slots through `tp_versions_used`) and
  `PyHeapTypeObject`, plus `PyNumberMethods`/`PySequenceMethods`/
  `PyMappingMethods`/`PyAsyncMethods`/`PyBufferProcs` (defined faithfully now;
  *dispatched* from in wave 2).

The header strategy: rather than maintain a hand-written faithful `Python.h`
(thousands of lines that must track CPython exactly), wave 1 makes the proof
extension build against the **host's own stock CPython 3.13 headers** and has
the host satisfy the symbols + layouts. WeavePy's `include/Python.h` is kept for
the existing WeavePy-header fixtures during the transition and is migrated
field-by-field to the faithful layout (the `PyTypeObject` widening is the first
step). A `build.rs` probe records the stock include dir
(`python3.13 -c "import sysconfig; print(sysconfig.get_path('include'))"`) when
present, gated so a host without CPython 3.13 simply skips the stock-headers
fixture (the WeavePy-header fixtures still run).

### WS2 - The object mirror bridge (~4K LOC)

The heart of the wave. A `mirror` module owns the bidirectional bridge:

- **`Object -> *mut PyObject` (mirror-out).** Given a native `Object`, return a
  pointer to a layout-faithful box. For immutable scalars the box is filled
  once: `Float -> PyFloatObject{ob_fval}`, `Int`/`Long -> PyLongObject` (encode
  i64/BigInt into the `lv_tag` + 30-bit `ob_digit[]` tail),
  `Bytes -> PyBytesObject{ob_size, ob_shash, ob_sval[]}`,
  `Str -> PyUnicode*` (choose ASCII/UCS1/UCS2/UCS4 by max code point; fill the
  `state` bit-field + inline character buffer; lazily populate the `utf8`
  cache on `PyUnicode_AsUTF8`). For `Tuple`/`List`, the `ob_item[]` slots are
  themselves mirrors, produced lazily.
- **`*mut PyObject -> Object` (mirror-in).** Resolve a pointer the extension
  hands back. Pointers WeavePy minted carry a back-reference to their native
  `Object` (stored in a header *prefix* immediately before the faithful
  `PyObject`, at a negative offset, so the public pointer stays byte-faithful);
  pointers the extension *created* (via `PyFloat_FromDouble` etc., which WeavePy
  implements, so they are also WeavePy mirrors) resolve the same way. A
  process-wide identity map (`PyObject* -> Object`) preserves `is` identity for
  mirrored containers and types.
- **Lifetime.** The mirror prefix holds the owning `Object` (an `Rc` clone), so
  while C holds a reference the native value is pinned. `Py_DecRef` to zero
  drops the prefix (and its `Rc`), running any registered destructor. Immortal
  mirrors (singletons, static types) never free.
- **Coherence.** Mutating container ops implemented in C
  (`PyList_SET_ITEM`/`PyList_Append`/`PyList_SetSlice`) write through to the
  native `List` store; read ops (`PyList_GET_ITEM`) return the cached element
  mirror. (The general mutable-after-share case for arbitrary slot writes is a
  wave-2 concern; wave 1 covers the constructor-then-fill pattern stock
  extensions use to *build* return values, which is the dominant case.)

### WS3 - Refcount + immortality fidelity (~0.5K LOC)

Move `IMMORTAL_REFCNT` to CPython 3.13's `_Py_IMMORTAL_REFCNT` (low 32 bits set,
i.e. `0xFFFF_FFFF` as a `Py_ssize_t` on 64-bit), and make `Py_IncRef`/`Py_DecRef`
match CPython's saturating-immortal semantics so a *stock inlined* refcount op
(which the host can't intercept) and the *function-call* form agree on the same
object. Audit the singleton/static initialisers (`_Py_NoneStruct`,
`_Py_TrueStruct`, the static type table) to the new sentinel.

### WS4 - High-frequency symbol surface over the bridge (~3K LOC)

Re-implement the load-bearing constructors/accessors so they speak the faithful
layout: `PyFloat_FromDouble`/`PyFloat_AsDouble`, `PyLong_FromLong`/
`FromLongLong`/`FromSsize_t`/`FromSize_t`/`AsLong`/`AsLongLong`/`AsSsize_t`,
`PyBool_FromLong`, `PyBytes_FromStringAndSize`/`AsString`/`Size`,
`PyUnicode_FromStringAndSize`/`FromString`/`AsUTF8AndSize`/`GetLength`,
`PyTuple_New`/`Pack`/`GetItem`/`SetItem`/`Size`, `PyList_New`/`Append`/
`GetItem`/`SetItem`/`Size`, `PyModule_Create2`/`AddObject`/`AddIntConstant`/
`AddStringConstant`, and the `PyArg_ParseTuple`/`Py_BuildValue` variadic core
(the existing `varargs.c` shim, re-pointed at the faithful constructors). Each
gets a faithful-layout unit test.

### WS5 - The stock-headers proof extension + loader path (~1.5K LOC)

- **`tests/capi_ext/_stockabi.c`** - a C extension authored to compile against
  **stock CPython 3.13 headers** (no WeavePy header), using a static
  `PyModuleDef`/`PyMethodDef`, `Py_RETURN_NONE`, the inlined macros
  (`PyFloat_AS_DOUBLE`, `PyList_GET_ITEM`, `Py_SIZE`, `Py_TYPE` comparisons
  against `&PyFloat_Type`/`&PyLong_Type`), and round-tripping every core type
  through a function (`roundtrip`, `list_sum`, `make_pair`, `echo_str`,
  `alloc_free_cycle`).
- **`build.rs`** compiles it with `cc -I$(python3.13 include)` when CPython 3.13
  is present, emitting an env var the test reads; absent CPython 3.13, the
  fixture is skipped (CI on a bare host still passes).
- **Loader.** `weavepy-capi::loader` already resolves `PyInit_<leaf>` and runs
  it under an `ActiveContext`; wave 1 confirms the returned faithful
  `PyModule_Create2` object bridges back correctly and the module's functions
  are callable from WeavePy.
- **`_minipip`/`ext_loader`.** Accept the stock `cp313-cp313-<plat>` and
  `cp313-abi3-<plat>` wheel tags (in addition to `weavepy-cp313-*`), so a stock
  binary wheel's `.so` is discovered and handed to the loader. (Resolving every
  symbol a *real* numpy wheel needs is waves 3-5; wave 1 wires the path and
  proves it on the core surface.)

### WS6 - Fixtures, integration tests, measured baseline (~1K LOC)

- `crates/weavepy-capi/tests/capi_stockabi.rs` - Rust integration tests that
  `dlopen` the stock-headers `.so` and assert the round trips (gated on the
  CPython-3.13 env var; skips cleanly when the host has no `python3.13`
  headers so a bare CI host still passes). **This is the delivered proof
  harness** — 9 cases covering inlined reads, type identity, the function API,
  C-side dealloc, and module init.
- A measured `expectations.toml` pass: wave 1 is C-API-only infrastructure, so
  no CPython `Lib/test` row flips and the sweep stays unchanged. _(A bundled
  Python-level `test_stock_abi_smoke.py` that imports the extension through the
  regrtest subprocess is deferred — it needs the loader on a subprocess import
  path, which is orthogonal to the ABI thesis the Rust harness already proves.)_

## Measured targets

The commit-acceptance bar for wave 1:

- A stock-CPython-3.13-headers extension (`_stockabi`) loads via `dlopen` and
  its functions run correctly under WeavePy - proven by `capi_stockabi.rs`.
- The faithful layouts carry compile-time size/offset assertions against the
  values in the host's stock headers.
- The existing WeavePy-header fixtures (`_smalltest`/`_ndarray`/`_numpylike`)
  and their tests stay green through the `PyTypeObject` widening + sentinel
  change.
- `cargo build --workspace`, `cargo fmt --check`, and
  `cargo clippy --workspace --all-targets -- -D warnings` are green; the
  regrtest sweep stays `--check` clean.

## Measured outcome

_As-landed (wave 1):_

- **Thesis proven.** `tests/capi_ext/_stockabi.c` is compiled by `build.rs`
  against the host's **stock CPython 3.13 headers** (resolved via
  `sysconfig.get_path('include')`), then `dlopen`ed and driven by
  `crates/weavepy-capi/tests/capi_stockabi.rs`. All 9 cases pass:
  - `inlined_float_read` — stock `PyFloat_AS_DOUBLE` (an inlined
    `*(double*)((char*)op + 16)`) reads a WeavePy float mirror.
  - `inlined_size_read` / `inlined_tuple_item_read` — inlined `Py_SIZE`
    and `PyTuple_GET_ITEM` read `ob_size`/`ob_item[]` at the right offsets.
  - `type_identity` — `Py_TYPE(o) == &PyFloat_Type` holds across the boundary.
  - `roundtrip_incref` — inlined `Py_INCREF`/`Py_DECREF` poke the head refcount.
  - `function_api` — `PyArg_ParseTuple`, the `Py*_From*` constructors, and
    `Py_BuildValue` work.
  - `c_side_dealloc` — a C-side `Py_DECREF`→0 reaches the external `_Py_Dealloc`,
    which reads `Py_TYPE(op)->tp_dealloc` at offset 48 and frees the mirror.
  - `module_loads_with_constants` — a stock `PyModuleDef`/`PyMethodDef`
    initialises and its module-level constants resolve.
- **No regression.** The full `weavepy-capi` suite is green (77 tests incl. the
  9 above), as are the WeavePy-header fixtures (`capi_loader`, `capi_ndarray`,
  `capi_wheel_endtoend`) and the `weavepy` behavioural `fixtures` harness,
  through the `PyTypeObject` widening (now 416 bytes, `tp_flags: u64`) and the
  `_Py_IMMORTAL_REFCNT = 0xFFFF_FFFF` sentinel change.
- **Clean gates.** `cargo build --workspace`, `cargo fmt`, and
  `cargo clippy -p weavepy-capi --all-targets` are green.
- **Regrtest unaffected.** The sweep is unchanged by this C-API-only work; the
  one observed divergence (`test_list::test_deopt_from_append_list`) reproduces
  identically with the changes stashed — it is a pre-existing `weavepy-cli`
  `-I -c` subprocess-isolation gap, not a binary-ABI regression, and is left for
  a separate CLI fix rather than a baseline rewrite.

## Non-goals / Drawbacks

- **Real numpy/pandas do not import in wave 1.** Their C-API dependency surface
  (much of it private/internal) is sequenced into waves 3-5. Wave 1's claim is
  narrowly "a stock-compiled extension using the core object/type/module surface
  runs", not "the headline wheels work".
- **The mirror layer has a cost.** Crossing an object into C now allocates (or
  looks up) a faithful mirror; hot extension loops pay for it. CPython pays
  zero here because its objects *are* the structs. Optimising the mirror
  (caching, arena allocation, avoiding round trips) is deferred; correctness
  first.
- **Mutable-aliasing coherence is partial in wave 1.** The constructor-then-fill
  build pattern is covered; arbitrary in-place mutation of a shared object via
  raw slot writes from C, concurrently observed from Python, is a wave-2
  coherence task.
- **rustls/OpenSSL-style "we are not CPython" gaps remain.** Anything that
  reads CPython *internal* (`pycore_*`) structures, or assumes the exact
  bytecode/`PyFrameObject`/`PyCodeObject` internals, is out of scope - those are
  not part of the documented extension ABI.
- **Free-threading and Windows are out of scope.** `Py_GIL_DISABLED` changes the
  object head; the `.pyd` loader path is unchanged from RFC 0022.
- **One ABI per binary.** Because the conversion replaces the exported surface,
  WeavePy-header extensions and stock extensions must agree on the faithful
  layout once the migration completes; during wave 1 both are kept green by
  widening the shared structs rather than forking the ABI.

## Alternatives

1. **Stay limited-API-only and require recompilation (status quo).** Lowest
   effort, but it can never load the pre-built wheels users actually install -
   the entire point of D1. Rejected by the option choice.
2. **abi3 / stable-ABI only.** Far smaller surface (opaque handles, no struct
   pokes beyond the head), and real abi3 wheels exist (PyO3/maturin). But it
   excludes numpy/pandas (which do not ship abi3) - i.e. the headline targets.
   This was offered as scope option D3 and not chosen; its symbol surface is a
   strict subset of the faithful ABI, so wave 1's core also advances it.
3. **Translate/patch wheels at install time.** Rewriting a wheel's machine code
   to call functions instead of inlining is a non-starter (it would be a JIT
   recompiler for arbitrary native code).
4. **Emulate via a shadow heap only at call boundaries (pure cpyext).** This is
   essentially the chosen design; the only WeavePy-specific twist is the
   negative-offset native back-reference prefix, which keeps the public
   `PyObject*` byte-faithful while letting the host resolve a pointer to its
   native `Object` in O(1) without a global lookup on the hot path.

## Prior art

- **PyPy `cpyext`.** The canonical "non-CPython runtime hosts CPython native
  extensions" layer. Its `to_cpyext`/`from_ref` object-mirror with an
  identity map is the direct model for WS2; its per-type "attach" handlers map
  onto our per-`Object`-variant mirror-out.
- **GraalPy's C-API (`Sulong`/native).** Maintains native mirror structs for
  CPython objects with a managed back-reference; validates the
  "faithful-layout mirror + identity map" approach at scale.
- **`pythoncapi-compat`** and CPython's own `Include/cpython/*.h` - the
  authoritative layouts the wave-1 structs are pinned to.
- **RFC 0022/0028/0029** - the in-tree foundation (loader, `PyType_FromSpec`,
  the import machinery, `_minipip`) this wave converts and builds on.

## Future work

- Waves 2-5 above (full type-suite dispatch, GC integration, the numpy C-API,
  real numpy from source, pandas/Cython).
- Mirror-layer performance (arena allocation, cache, fewer round trips).
- A faithful vendored `Python.h` (or adopting CPython's headers wholesale) once
  the exported ABI is fully migrated, so extensions need neither WeavePy's
  header nor a separately-installed CPython.
- Windows `.pyd` and `Py_GIL_DISABLED` layouts.

