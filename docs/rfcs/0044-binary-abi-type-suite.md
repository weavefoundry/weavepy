# RFC 0044: CPython 3.13 binary-ABI compatibility (cpyext) - wave 2: the full type-suite round trip + GC integration

- **Author**: WeavePy core
- **Status**: Accepted
- **Part of**: the D1 binary-ABI arc whose roadmap lives in
  [RFC 0043](0043-cpython-binary-abi.md). RFC 0043 is the umbrella/roadmap
  RFC; this is the detailed-design RFC for **wave 2**.
- **Builds on**: RFC 0043 (wave 1 - the layout-faithful object mirror, the
  byte-faithful `PyTypeObject`, the immortal-refcount sentinel, the
  stock-headers proof harness), RFC 0028 (the `PyType_FromSpec` slot surface,
  the `SlotTable`, the `dunder_shim` bridge, vectorcall, PEP 3118 buffers),
  RFC 0024 (the generational tracing cycle collector).

## Summary

Wave 1 made WeavePy *objects* byte-faithful so a stock extension's **inlined
field reads** (`PyFloat_AS_DOUBLE`, `Py_SIZE`, `PyTuple_GET_ITEM`) land on real
CPython-shaped memory. That is the *data* direction. Wave 2 lands the
*behaviour* direction: when Python code drives an object whose type was
**defined by a stock extension**, WeavePy must read the type's method suites and
`tp_*` slots at their faithful offsets and *call into the extension*.

The load-bearing discovery that scopes this wave: **`PyType_Ready` is currently
a no-op**, and the method-suite structs (`PyNumberMethods`, …) are modelled as
opaque byte blobs. So the single most common way a real C extension defines a
type - a statically-initialised `PyTypeObject` with `tp_as_number = &my_number`,
`tp_call = …`, `tp_richcompare = …`, then `PyType_Ready(&MyType)` - does
*nothing* on WeavePy today: no bridge, no dispatch, no instantiation. Every
in-tree fixture sidesteps this by using `PyType_FromSpec` (the `PyType_Slot[]`
array path RFC 0028 built); stock wheels overwhelmingly do **not**.

Wave 2 therefore:

1. **Spells out the five method suites** (`PyNumberMethods` / `PySequenceMethods`
   / `PyMappingMethods` / `PyAsyncMethods` / `PyBufferProcs`) field-by-field,
   byte-faithful and offset-asserted against the host's stock 3.13 headers.
2. **Makes `PyType_Ready` real**: it harvests a faithfully-laid-out
   `PyTypeObject` plus its method suites into the same `SlotTable` →
   `dunder_shim` → `Rc<TypeObject>` machinery that `PyType_FromSpec` already
   uses, then bridges and registers the type. The two type-definition styles
   converge on one finalisation path.
3. **Fills the dispatch gaps** the shim layer didn't cover: descriptors
   (`tp_descr_get`/`tp_descr_set` → `__get__`/`__set__`), the async suite
   (`am_await`/`am_aiter`/`am_anext` → `__await__`/`__aiter__`/`__anext__`),
   and `tp_new` → `__new__`.
4. **Integrates the GC**: a readied type that advertises `tp_traverse`/`tp_clear`
   participates in WeavePy's generational cycle collector.

The acceptance proof is a second hermetic fixture compiled against the **stock
CPython 3.13 headers** that defines its types the static-`PyTypeObject` way and
exercises numeric, sequence, mapping, comparison, call, iteration, descriptor,
and GC behaviour through WeavePy's dispatcher.

Faithful *inline instance storage* (a stock type reading `self->field` at a
fixed offset in its own `tp_basicsize` block) is explicitly **out of scope** and
deferred to wave 3, where real numpy forces it; see Non-goals.

## Motivation

A C extension does two distinct things with the runtime:

- It **reads object data** through inlined accessors. Wave 1 handled this with
  the mirror: a WeavePy `float` crossing into C looks byte-for-byte like a
  `PyFloatObject`.
- It **defines behaviour** - new types whose operators, call protocol,
  iteration, comparison, and lifecycle are C functions the runtime must invoke
  at the right moments. This is the half wave 2 lands.

The behaviour half is what makes an extension a *first-class participant* rather
than a passive data producer. `numpy.ndarray.__add__`, `decimal.Decimal`'s
arithmetic, `datetime`'s comparisons, a Cython class's `__call__` - all of these
are C slots hanging off a type the extension defined. Until WeavePy reads those
slots and dispatches to them, `import numpy` can at best hand back arrays nobody
can compute with.

RFC 0028 already built the dispatch *mechanism* for one definition style
(`PyType_FromSpec`): decode a `PyType_Slot[]` array into a `SlotTable`, then
synthesise `__add__`/`__call__`/… shims (`dunder_shim`) into the type dict so
the VM's ordinary dunder dispatch reaches the C function. That machinery is
sound and stays. What it lacks is a *source*: the other - and far more common -
definition style, where the slots live in a statically-initialised
`PyTypeObject` and its method-suite sub-structs, finalised by `PyType_Ready`.
Wave 2 adds that source and reuses the mechanism.

## The central problem, precisely

Consider the canonical stock type definition (verbatim from countless real
extensions and Cython output):

```c
static PyNumberMethods Vec_as_number = {
    .nb_add = Vec_add,            /* offset 0   in PyNumberMethods */
    .nb_multiply = Vec_mul,       /* offset 16  */
};
static PyTypeObject VecType = {
    PyVarObject_HEAD_INIT(NULL, 0)
    .tp_name = "vec.Vec",
    .tp_basicsize = sizeof(VecObject),
    .tp_as_number = &Vec_as_number,   /* offset 96  in PyTypeObject */
    .tp_richcompare = Vec_richcompare,/* offset 200 */
    .tp_call = Vec_call,              /* offset 128 */
    .tp_new = PyType_GenericNew,
    .tp_init = Vec_init,
};
/* in module init: */
if (PyType_Ready(&VecType) < 0) return NULL;
PyModule_AddObject(m, "Vec", (PyObject *)&VecType);
```

For `Vec(1) + Vec(2)` to work, WeavePy must, at `PyType_Ready` time:

1. Read `VecType.tp_as_number` (offset 96) → a `PyNumberMethods*`, then read
   `nb_add` (offset 0 within it). The struct must be **spelled out** so offset
   96-then-0 resolves to `Vec_add` and not a misaligned blob byte.
2. Read the direct slots `tp_richcompare` (200), `tp_call` (128), `tp_init`
   (296), `tp_new` (312), `tp_iter`/`tp_iternext`, `tp_getattro`/`tp_setattro`,
   `tp_descr_get`/`tp_descr_set`, `tp_traverse`/`tp_clear`, `tp_methods`,
   `tp_members`, `tp_getset`, `tp_doc`, `tp_base`.
3. Fold all of them into a `SlotTable`, build the bridged `Rc<TypeObject>` with
   the right base/MRO, synthesise the dunder shims, and register the type so
   `type(Vec(1)) is Vec` round-trips.

Today step 1 is impossible (opaque suites), step 2 never happens (`PyType_Ready`
returns 0 without looking at `*t`), and step 3 only runs for `PyType_FromSpec`.
Wave 2 closes all three.

The mirror image - WeavePy *calling out* through these slots - already exists
once the `SlotTable` is populated, because the `dunder_shim` `BuiltinFn`
closures marshal `Object → *mut PyObject`, invoke the slot under
`interp::ensure_active`, and bridge the result back. Wave 2's job is to *fill
the table from the faithful struct* and to *extend the shim coverage* to the
slots RFC 0028 left out.

## CPython reference

Targets **CPython 3.13** as installed on the build host. The method-suite
layouts wave 2 pins (read out of the host's stock headers with an
`offsetof`/`sizeof` probe on the 64-bit LP64 / arm64 + x86-64 ABI):

| Suite | Size | Key offsets (bytes) |
|-------|------|---------------------|
| `PyNumberMethods` | 288 | `nb_add` 0, `nb_subtract` 8, `nb_multiply` 16, `nb_remainder` 24, `nb_divmod` 32, `nb_power` 40, `nb_negative` 48, `nb_bool` 72, `nb_int` 128, `nb_reserved` 136, `nb_float` 144, `nb_inplace_add` 152, `nb_floor_divide` 232, `nb_true_divide` 240, `nb_index` 264, `nb_matrix_multiply` 272 |
| `PySequenceMethods` | 80 | `sq_length` 0, `sq_concat` 8, `sq_repeat` 16, `sq_item` 24, `was_sq_slice` 32, `sq_ass_item` 40, `sq_contains` 56, `sq_inplace_concat` 64, `sq_inplace_repeat` 72 |
| `PyMappingMethods` | 24 | `mp_length` 0, `mp_subscript` 8, `mp_ass_subscript` 16 |
| `PyAsyncMethods` | 32 | `am_await` 0, `am_aiter` 8, `am_anext` 16, `am_send` 24 |
| `PyBufferProcs` | 16 | `bf_getbuffer` 0, `bf_releasebuffer` 8 |

Note the two reserved holes the faithful structs must preserve:
`PyNumberMethods` keeps `nb_reserved` (offset 136, historically `nb_long`), and
`PySequenceMethods` keeps `was_sq_slice` (32) and `was_sq_ass_slice` (48). The
`tp_*` slot offsets in `PyTypeObject` are unchanged from wave 1 (e.g.
`tp_as_number` 96, `tp_call` 128, `tp_richcompare` 200, `tp_descr_get` 272,
`tp_traverse` 184, `tp_init` 296, `tp_new` 312) and remain machine-checked in
`layout::PyTypeObjectFull`.

## Current baseline (measured starting point)

- Wave 1 green: the `_stockabi` proof (9 cases) plus the full `weavepy-capi`
  suite (77 tests), the WeavePy-header fixtures, and the behavioural `fixtures`
  harness.
- `PyType_FromSpec` types dispatch operators/call/iter/compare via `dunder_shim`
  (RFC 0028); the buffer protocol and vectorcall dispatch directly off the
  `SlotTable`.
- `PyType_Ready` is a no-op; the method suites are opaque blobs; `tp_descr_*`,
  `am_*`, and `tp_new` have no shim; no C `tp_traverse`/`tp_clear` reaches the
  collector.

## Roadmap context

This is wave 2 of the five-wave D1 arc defined in
[RFC 0043 §Roadmap](0043-cpython-binary-abi.md). Wave 1 landed the object/type
layouts and the mirror. Wave 3 is the numpy C-API surface (and the inline
instance storage this wave defers); wave 4 builds real numpy from source; wave 5
is pandas/Cython + the wheel matrix.

## Detailed design (wave 2)

Seven workstreams in dependency order.

### WS1 - Spell out the method suites (~0.6K LOC)

Replace the `opaque_suite!` blobs in `crate::layout` with `#[repr(C)]` structs
that name every slot, each typed `*mut c_void` (the ABI is pointer-width and the
harvest stores the raw pointer in the `SlotTable`, casting to the concrete
`unsafe extern "C" fn` only at the call site - matching how `PyTypeObject`
already types its `tp_*` slots), and pin them with
`const _: () = assert!(size_of/offset_of ...)` against the values in the table
above. The reserved holes (`nb_reserved`, `was_sq_slice`, `was_sq_ass_slice`)
are named fields so the asserts cover the whole struct, not just the slots we
read. The live `crate::types::PyTypeObject` keeps `tp_as_number` (etc.) typed as
`*mut c_void` for ABI width; the harvest casts to the spelled-out suite.

### WS2 - A real `PyType_Ready` + a shared finalisation path (~1.5K LOC)

Factor the back half of `PyType_FromMetaclass` - "given a `SlotTable`, a doc,
method/getset/member descriptor lists, and a base list, build the
`Rc<TypeObject>`, synthesise dunders, box + register" - into a
`finalize_type(...)` helper. Then:

- **`harvest_faithful(ty) -> HarvestedSlots`** reads a faithfully-laid-out
  `PyTypeObject`: every direct `tp_*` function slot into its canonical
  `Py_tp_*` id, and each non-null method suite (`tp_as_number` →
  `PyNumberMethods`, …) decomposed into its `Py_nb_*`/`Py_sq_*`/`Py_mp_*`/
  `Py_am_*`/`Py_bf_*` ids. `tp_methods`/`tp_getset`/`tp_members`/`tp_doc`/
  `tp_base` are harvested for the dict + base resolution.
- **`PyType_Ready(t)`** becomes: if `t` already has a `bridge` (a WeavePy static
  built-in or an already-readied type), return 0; otherwise harvest `*t`,
  resolve the base from `tp_base` (defaulting to `object`), `finalize_type`,
  then **write the bridge and the `SlotTable` back into the caller's `*t`** (the
  extension keeps using *its* `&MyType` pointer, so the bridge must live on that
  struct, not only on a fresh box) and register it. Set `Py_TPFLAGS_READY`.

`PyType_FromMetaclass` is refactored to call `finalize_type` so both paths stay
identical from the dunder-synthesis point on.

### WS3 - Close the dunder-shim gaps (~0.8K LOC)

`install_dunder_shims` gains coverage for the slots RFC 0028 omitted, all
following the established `BuiltinFn`-closure pattern:

- **Descriptors**: `Py_tp_descr_get` → `__get__(self, obj, type)`,
  `Py_tp_descr_set` → `__set__(self, obj, value)`. These make an extension-
  defined descriptor work when placed on a class.
- **Async**: `Py_am_await` → `__await__`, `Py_am_aiter` → `__aiter__`,
  `Py_am_anext` → `__anext__` (unary slots returning an iterator/awaitable).
- **Construction**: `Py_tp_new` → `__new__`. The shim calls the C `tp_new(type,
  args, kwds)` and bridges the result; types without a custom `tp_new` keep the
  VM's default instance creation.

### WS4 - GC integration (~0.7K LOC)

Register one traverse handler (`gc_trace::register_traverse`) that matches an
`Object::Instance` whose class bridges to a readied type carrying a non-null
`tp_traverse`, and a clear path for `tp_clear`. At collection time the handler:

- materialises the instance as a borrowed `*mut PyObject`,
- calls the C `tp_traverse(self, visit_trampoline, arg)` where
  `visit_trampoline` is an `extern "C"` `visitproc` that bridges each visited
  child `*mut PyObject` back to an `Object` and forwards it to the collector's
  `&mut dyn FnMut(&Object)` (threaded through the `void *arg`),
- and, when breaking a cycle, invokes `tp_clear(self)`.

Because `register_traverse` takes plain `fn` pointers, the handler re-derives
the type's slots from the object at call time (no captured state). Re-entrancy
into C during collection is bounded by `interp::ensure_active`, matching the
existing dunder-call discipline. The clear path needs a companion VM hook -
`gc_trace::register_clear` (mirroring `register_traverse`) - invoked from
`clear_object_fields` *before* the instance dict is wiped, so a `tp_clear` that
reads its own identity back out of `__dict__` still sees it.

This wave also lands the **GC allocation + tracking C-API** a stock
`Py_TPFLAGS_HAVE_GC` type needs end-to-end: `_PyObject_GC_New` /
`_PyObject_GC_NewVar` (storage via the existing `PyType_GenericAlloc` model),
`PyObject_GC_Track` / `PyObject_GC_UnTrack` (enrol/withdraw the bridged
`Object::Instance` with the collector, over a new `gc_trace::untrack`
convenience), `PyObject_GC_IsTracked`, and `PyObject_GC_Del` (untrack + release
the box). All are added to the force-link table so a `dlopen`'d extension
resolves them against the host.

### WS5 - Instantiation + state for readied types (~0.5K LOC)

So a readied stock type is actually *constructible*: `PyType_GenericAlloc`, when
its `ty` bridges to a native `TypeObject`, initialises the box payload to a
real `Object::Instance(PyInstance::new(cls))` (rather than `Object::None`), so
`tp_new`/`tp_init` and `PyObject_SetAttrString` operate on a genuine instance
whose `__dict__` round-trips. Combined with the `__new__`/`__init__` shims, this
lets a stock type construct instances and store per-instance state (via its
`__dict__`, the same side-channel the in-tree fixtures use). Inline
`tp_basicsize` field storage remains a wave-3 concern (Non-goals).

### WS6 - The stock-headers proof (~1K LOC)

`tests/capi_ext/_stocktype.c`, compiled by `build.rs` against the **stock
CPython 3.13 headers** (like `_stockabi`, gated + self-skipping when absent),
defines its types the static-`PyTypeObject` way and calls `PyType_Ready`. The
breadth is split across small, focused types (each storing its payload in a
side-allocated `*Core` block whose address is stashed in
`self.__dict__["_core_addr"]`, the `_ndarray` pattern):

- **`Vec2`** - `tp_as_number` (`nb_add`/`nb_subtract`) + `tp_richcompare`
  (`==`/`!=`) + `tp_repr` + `tp_init`. Also the *construct-a-readied-type-by-
  calling-it-from-C* path: `nb_add`/`nb_subtract` build their result with
  `PyObject_CallFunction((PyObject *)&Vec2_Type, "ll", …)` (re-entrantly, from
  inside a slot), and the module-level `make_vec2()` does the same at top level.
- **`Seq`** - `tp_as_sequence` (`sq_length`/`sq_item`) + `tp_as_mapping`
  (`mp_length`/`mp_subscript`) + `tp_iter`/`tp_iternext` (a self-iterating
  `[0, n)` view).
- **`Adder`** - `tp_call` (`Adder(base)(x) == base + x`).
- **`Const`** - a data descriptor with `tp_descr_get`/`tp_descr_set`.
- **`Aw`** - `tp_as_async` (`am_await`/`am_aiter`/`am_anext`), a hermetic
  *dispatch* proof (no event loop): the slots return integer sentinels / `self`
  so the test can confirm the synthesised `__await__`/`__aiter__`/`__anext__`
  reach the genuine `PyAsyncMethods`.
- **`Proxy`** - custom attribute access: `tp_getattro` synthesises a value for
  one name and falls back to `PyObject_GenericGetAttr` otherwise (no recursion -
  the generic path does not re-enter `getattro`); `tp_setattro` records the
  write in a module global and then stores it via `PyObject_GenericSetAttr`, so
  the value round-trips back out.
- **`Node`** - a `Py_TPFLAGS_HAVE_GC` container whose single child reference
  lives in C-managed memory (the side core, invisible to the dict walker) and
  is surfaced/broken only through `tp_traverse`/`tp_clear`; allocated via
  `PyObject_GC_New` and enrolled with `PyObject_GC_Track`.

`crates/weavepy-capi/tests/capi_stocktype.rs` loads the module and asserts each
behaviour through WeavePy's evaluator (`__add__`/`__sub__`, `__eq__`, `len`,
subscription, iteration, call, descriptor get/set, async
`__await__`/`__aiter__`/`__anext__`, custom `__getattribute__`/`__setattr__`,
and construction by calling the readied type object from C), and that a two-node
`Node` cycle held together *only* by the C-side child pointers is reclaimed by a
full `gc.collect()` - observed through C counters showing `tp_traverse`/
`tp_clear` fired and the live-node count returned to zero.

### WS7 - Measured baseline (~0.2K LOC)

Run the `weavepy-capi` suite, the WeavePy-header fixtures, the behavioural
`fixtures` harness, and a regrtest slice; ensure `cargo build --workspace`,
`cargo fmt`, and `cargo clippy -p weavepy-capi --all-targets` are green; fill in
the Measured outcome.

## Measured targets

The commit-acceptance bar for wave 2:

- A stock-CPython-3.13-headers extension (`_stocktype`) that defines its types
  as static `PyTypeObject`s + method suites and calls `PyType_Ready` loads via
  `dlopen` and dispatches numeric, sequence, mapping, comparison, call,
  iteration, descriptor, async, and custom-attribute-access behaviour correctly
  under WeavePy - including constructing a readied type by calling the type
  object from C (`PyObject_CallFunction`) - proven by `capi_stocktype.rs`.
- A `Py_TPFLAGS_HAVE_GC` type's `tp_traverse`/`tp_clear` participate in the
  cycle collector: a constructed reference cycle through such instances is
  reclaimed by `gc.collect()`.
- The faithful suites carry compile-time size/offset assertions against the
  host's stock headers.
- The wave-1 `_stockabi` proof, the WeavePy-header fixtures
  (`_smalltest`/`_ndarray`/`_numpylike`), and their tests stay green through the
  suite spell-out and the `PyType_Ready` change.
- `cargo build --workspace`, `cargo fmt`, and
  `cargo clippy -p weavepy-capi --all-targets` are green; the regrtest sweep is
  unchanged.

## Measured outcome

Landed as designed, then hardened (see below). ~1.2K LOC of production change
(≈964 lines across `build.rs`, `dunder_shim.rs`, `force_link_table.rs`,
`genericalloc.rs`, `interp.rs`, `layout.rs`, `slottable.rs`, `types.rs`, and
`gc_trace.rs`, plus a 242-line `gc_bridge.rs`) and ~1.2K LOC of proof
(`_stocktype.c` + `capi_stocktype.rs`). The hardening pass additionally
corrected `varargs.c` (the `Py_VaBuildValue` multi-unit bug, below).

- **Stock-headers proof green.** `tests/capi_ext/_stocktype.c`, compiled against
  the host's stock CPython 3.13 headers (3.13.13 on the dev box), `dlopen`s into
  WeavePy and `capi_stocktype.rs` passes **11/11**: a module of seven static
  `PyTypeObject`s finalised by `PyType_Ready` dispatches numeric
  (`nb_add`/`nb_subtract`), rich comparison, sequence (`sq_length`/`sq_item`),
  mapping (`mp_subscript`), iteration (`tp_iter`/`tp_iternext`), call (`tp_call`),
  descriptor (`tp_descr_get`/`tp_descr_set`), async
  (`am_await`/`am_aiter`/`am_anext`), and custom attribute access
  (`tp_getattro`/`tp_setattro`) behaviour through WeavePy's evaluator.
- **Constructing a readied type by calling it from C works.** `Vec2` instances
  are built by `PyObject_CallFunction((PyObject *)&Vec2_Type, "ll", …)` - both at
  the top level (`make_vec2()`) and re-entrantly from inside `nb_add`/
  `nb_subtract`. Hardening this surfaced and fixed a real, general bug:
  `Py_VaBuildValue` built only the *first* unit of a multi-unit format string,
  so **every** `PyObject_CallFunction`/`PyObject_CallMethod` with a multi-arg
  format (`"ll"`, `"OO"`, …) silently dropped all arguments past the first and
  called with a 1-tuple. It now shares `Py_BuildValue`'s multi-unit tuple logic.
  (This is squarely on the path numpy & friends use to build call arguments, so
  the fix matters well beyond type construction.)
- **GC through C-managed memory works.** A two-node cycle whose only edges live
  in C side allocations (invisible to the dict walker) is reclaimed by
  `gc_trace::collect_all()`: the C counters confirm `tp_traverse` and `tp_clear`
  fired and the live-node count returned to zero. The nodes allocate via
  `PyObject_GC_New` and enrol via `PyObject_GC_Track`.
- **No regressions.** The full `weavepy-capi` suite is green - **88 tests**
  across the library unit tests and the `capi_buffer` / `capi_loader` /
  `capi_ndarray` / `capi_numpylike` / `capi_wheel_endtoend` fixtures, plus the
  wave-1 `capi_stockabi` proof (**9/9**, unchanged through the suite spell-out
  and the `PyType_Ready` overhaul). The `capi_numpylike` / `capi_wheel_endtoend`
  fixtures - which lean on multi-arg `PyObject_CallFunction`/`PyObject_CallMethod`
  - confirm the `Py_VaBuildValue` fix is a strict improvement. The method-suite
  structs carry compile-time size/offset assertions against the faithful layout.
- **Tooling green.** `cargo build` (workspace, incl. the release `weavepy`
  binary), `cargo fmt --all`, and `cargo clippy --workspace --all-targets` are
  clean.
- **Conformance unchanged.** The GC-relevant regrtest slice (subprocess mode
  against the release `weavepy`) is unaffected: bundled `test_gc_basic` /
  `test_ws4_gc_cascade` and CPython's `Lib/test/test_gc.py` pass, and the
  `extension` / `weakref` / `finalizers` / `objmodel` / `iter_gen` slices report
  **0 unexpected** results. (The VM-side `register_traverse`/`register_clear`
  hooks are inert until `weavepy-capi` initialises, so a pure-VM run is
  byte-for-byte unchanged.)

## Non-goals / Drawbacks

- **Inline `tp_basicsize` instance storage is deferred to wave 3.** A stock type
  that reads `self->field` at a fixed byte offset inside its own instance block
  (the `PyArrayObject` shape, and any type using `tp_members` with `T_INT`/
  `T_DOUBLE` at an offset) is *not* supported here. Wave 2 instances remain
  `Object::Instance` values that store state in `__dict__` (the same model the
  in-tree fixtures already use). This is a deliberate cut: the roadmap places
  inline instance layout with the numpy work (wave 3), where it is unavoidable,
  and keeping it out lets wave 2 land green without reworking the instance
  representation. The consequence is that wave 2 proves *slot dispatch* for
  stock-defined types, not *arbitrary stock instance layouts*.
- **Multiple inheritance from two C types, metaclass slots, and `__slots__`
  offset packing** are not addressed beyond what `TypeObject::new_user`
  linearisation already gives.
- **The shim layer has a cost.** Each operator on a readied type crosses
  Rust→C→Rust with `Object`↔`*mut PyObject` marshalling. Hot loops pay for it;
  a faithful-slot fast path (calling the C slot without rebuilding the dunder
  closure) is future work.
- **`tp_traverse` during collection re-enters C.** The handler runs extension
  code while the collector holds its state; we bound this with the existing
  `ensure_active` discipline and only traverse (read child edges), but a
  badly-behaved `tp_traverse` that mutates the object graph mid-collection is
  outside what we defend against (CPython has the same hazard).

## Alternatives

1. **Require `PyType_FromSpec` (status quo).** Works only for extensions that
   adopt the limited-API spec style; excludes the static-`PyTypeObject` +
   `PyType_Ready` majority and all of numpy's core. Rejected - it is exactly the
   gap.
2. **Translate static type defs into a synthetic `PyType_Spec` and reuse
   `PyType_FromSpec` verbatim.** Tempting, but a `PyType_Spec` cannot express
   everything a readied static type carries (e.g. an already-`PyType_Ready`'d
   base by pointer, `tp_dictoffset`/`tp_weaklistoffset`), and `PyType_Ready`
   must mutate the *caller's* struct in place. Harvesting into the shared
   `SlotTable` finalisation path gives the reuse without the impedance mismatch.
3. **Lazily harvest slots on first dispatch instead of at `PyType_Ready`.**
   Defers cost but breaks `type(x)` identity and `PyModule_AddObject(m, "T",
   &T)` (which needs the bridge present immediately). Rejected.

## Prior art

- **PyPy `cpyext`** readies static types by reading their slots into the
  interpreter's `W_TypeObject`, synthesising app-level methods - the direct
  model for WS2.
- **GraalPy** harvests `PyTypeObject` slots into managed type info at
  `PyType_Ready`, including the method suites; validates the approach at scale.
- **CPython `Objects/typeobject.c::type_ready`** - the authoritative semantics
  for slot inheritance, `tp_new`/`tp_alloc` defaulting, and the
  `tp_as_number`/`…` fix-up that WS2 mirrors (a faithful subset).
- **RFC 0028** - the `SlotTable`/`dunder_shim`/vectorcall/buffer machinery wave
  2 fills from a new source.

## Future work

- Inline `tp_basicsize` instance storage with a negative-offset native
  back-reference (extending the wave-1 mirror to user-type instances) - wave 3,
  for numpy's `PyArrayObject`.
- Slot inheritance fix-up for subclassing a C type from Python (inherit the
  base's suites unless overridden), matching `type_ready`'s `inherit_slots`.
- A faithful-slot fast path that bypasses the dunder-closure rebuild on hot
  numeric loops.
- `tp_members`/`T_*` descriptor reads/writes against inline storage (pairs with
  wave 3).



