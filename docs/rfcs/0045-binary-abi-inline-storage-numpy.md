# RFC 0045: CPython 3.13 binary-ABI compatibility (cpyext) - wave 3: faithful inline instance storage + the numpy array C-API surface

- **Author**: WeavePy core
- **Status**: Accepted
- **Part of**: the D1 binary-ABI arc whose roadmap lives in
  [RFC 0043](0043-cpython-binary-abi.md). RFC 0043 is the umbrella/roadmap
  RFC; this is the detailed-design RFC for **wave 3**.
- **Builds on**: RFC 0043 (wave 1 - the layout-faithful object mirror with its
  negative-offset prefix, the byte-faithful `PyTypeObject`, the immortal-refcount
  sentinel), RFC 0044 (wave 2 - the full type-suite round trip, real
  `PyType_Ready`, the `SlotTable` -> `dunder_shim` finalisation path, GC
  integration), RFC 0029 (the numpy-grade end-to-end path - the PEP 3118 buffer
  protocol, the complete `PyCapsule_*` surface incl. `PyCapsule_Import`'s dotted
  semantics, the `_numpylike.c` fixture).

## Summary

Wave 1 made WeavePy *objects* byte-faithful (a `float` crossing into C looks like
a `PyFloatObject`). Wave 2 made *behaviour* faithful (a stock-defined type's
`tp_*` slots and method suites dispatch through WeavePy's VM). Both waves left one
load-bearing thing deferred, and wave 2 named it explicitly as a wave-3
non-goal: **inline `tp_basicsize` instance storage** - a stock type reading
`self->field` at a fixed byte offset *inside its own instance block*.

This is exactly the shape numpy's `PyArrayObject` is built on. `PyArray_DATA(arr)`
expands (in the stock, inlined header) to `((PyArrayObject *)arr)->data` - a read
at offset 16; `PyArray_NDIM`, `PyArray_DIMS`, `PyArray_STRIDES` are the same. A
runtime that cannot present a stock array instance as a stable C struct whose
fields live at the right offsets cannot host numpy at all, no matter how complete
its symbol table is. Wave 3 closes that gap and then lands the numpy-specific
*C-API surface* that rides on top of it.

Wave 3 therefore:

1. **Gives C-extension instances a single, stable, layout-faithful body.** An
   instance of a stock-defined type (a `PyType_FromSpec` heap type or a
   `PyType_Ready`'d static type) that declares inline fields
   (`tp_basicsize > sizeof(PyObject)`) is materialised once into a
   `tp_basicsize`-sized faithful block - `[PyObject head | inline fields | inline
   var-data]` - reusing wave 1's negative-offset prefix. The body is **owned by
   the native instance** and presents the **same pointer** on every crossing
   into C, so a field written in one C call is still there in the next.
2. **Implements `tp_members` for real.** `T_INT` / `T_DOUBLE` / `T_OBJECT` /
   `T_LONGLONG` / ... members project to/from their declared offset *in that
   faithful body*, so `obj.field` (Python) and `self->field` (C) read and write
   the same bytes - the wave-2 stub that returned `None` is replaced.
3. **Lands the numpy array interchange + C-API-capsule pattern.** The
   `__array_interface__` / `__array_struct__` protocols (so a consumer can read a
   producer's array buffer without numpy linked), and the *array-C-API capsule*
   shape (`import_array()` -> `PyCapsule_Import("pkg._core._multiarray_umath._ARRAY_API")`
   -> a `void **` function-pointer table) that every numpy-consuming extension
   uses, proven by a producer/consumer pair. Making this work required closing a
   latent gap: a `PyCapsule` collapsed to `None` when it crossed into the VM (a
   module dict / attribute), so wave 3 gives it an identity-stable
   `Object::Capsule` *soul* that round-trips back to the same box.

The acceptance proof is a third hermetic fixture, `_stockarray.c`, compiled
against the **stock CPython 3.13 headers**, defining a `PyArrayObject`-shaped
type with real inline fields and `tp_members`, publishing an array-C-API capsule,
and exposing `__array_interface__` - exercised end-to-end through WeavePy.

Building **real numpy** from source, the full ufunc-loop registration machinery,
and the complete private `_multiarray_umath` symbol tail remain **wave 4** (see
Non-goals); wave 3 makes them *possible* by landing the instance-layout
foundation and the interchange surface they stand on.

## Motivation

A stock C extension's relationship to *its own* instances is the one place
WeavePy's mirror model had not yet reached. Waves 1-2 covered the two directions
that flow *through* the type system:

- **Reading built-in data** (wave 1): a WeavePy `float`/`tuple`/`str` crossing
  into C is a faithful mirror.
- **Dispatching to C behaviour** (wave 2): a stock-defined type's operators,
  call, iteration, comparison, descriptors, and GC hooks fire through the VM.

But the *instance* of a stock type was still a WeavePy `Object::Instance` whose
storage was a Rust `PyInstance` (a `__dict__`, slots, a class pointer). When such
an instance crossed into C, wave 1/2 minted a fresh `PyObjectBox` - a Rust
payload, not a C struct - and minted a **new one on every crossing**. So:

- `((MyType *)self)->field` read a Rust enum, not the field.
- A field written by C in one call was on a box that was thrown away; the next
  call got a different box with different bytes.
- `tp_members` (which names a field by *offset*) had nothing real to point at, so
  wave 2 left it a `None`-returning stub.

The in-tree fixtures sidestep this with the `_core_addr` idiom: `malloc` a side
struct, stash a `PyLong`-encoded pointer to it in `self.__dict__["_core_addr"]`,
and chase that pointer on every method. That works for a hand-written fixture but
it is **not** what a stock wheel does, and it is the diametric opposite of
numpy's hot path: numpy reads `arr->data` directly, inlined, with zero function
calls and zero dict lookups. There is no way to satisfy that reader except to
make the bytes at `self + offset` *be* the field - the same thesis wave 1 applied
to `float`, now applied to an extension's own instances.

Down-tree this unblocks the headline target. numpy's `_multiarray_umath`:

- defines `PyArrayObject` as a C struct read through inlined `PyArray_*` macros
  (the instance-layout problem wave 3 solves);
- publishes its entire C-API as a `void **` table wrapped in a capsule named
  `numpy._core._multiarray_umath._ARRAY_API`, which every consumer (`scipy`,
  `pandas`, a user's Cython module) pulls in via the `import_array()` macro
  (the capsule-pattern problem wave 3 proves);
- interoperates with non-numpy producers through `__array_interface__` /
  `__array_struct__` (the interchange problem wave 3 lands).

## The central problem, precisely

Consider the canonical stock array type (verbatim shape from numpy and a hundred
Cython extensions):

```c
typedef struct {
    PyObject_HEAD
    double      *data;        /* offset 16 */
    Py_ssize_t   size;        /* offset 24 */
    int          ndim;        /* offset 32 */
    Py_ssize_t   shape[NPY_MAXDIMS]; /* offset 40 ... */
} ArrayObject;

static PyMemberDef Array_members[] = {
    {"size", T_PYSSIZET, offsetof(ArrayObject, size), READONLY, NULL},
    {"ndim", T_INT,      offsetof(ArrayObject, ndim), READONLY, NULL},
    {NULL}
};
static PyTypeObject ArrayType = {
    PyVarObject_HEAD_INIT(NULL, 0)
    .tp_name = "mod.Array",
    .tp_basicsize = sizeof(ArrayObject),   /* > sizeof(PyObject) */
    .tp_members = Array_members,
    /* ... */
};
```

For `a = mod.Array(...); a.size` (Python) and `((ArrayObject *)a)->data`
(C, inlined) to agree, WeavePy must, for the lifetime of that one instance:

1. Allocate **one** `tp_basicsize`-byte block laid out as the C struct, zeroed,
   with `ob_refcnt`/`ob_type` at offsets 0/8.
2. Hand C the **same** pointer to that block every time the instance crosses the
   boundary (so writes persist and `a is a` survives a round trip).
3. Resolve that pointer **back** to the native `Object::Instance` (so the
   instance's `__dict__`, its class, its slot dispatch still work).
4. Read/write `tp_members` at their declared offsets *in that block*.
5. Free the block - exactly once - when the instance dies.

Today step 1 reserves the bytes (`PyType_GenericAlloc` already sizes to
`tp_basicsize`) but lays a `PyObjectBox` over them; step 2 fails (fresh box per
crossing); step 4 is a stub. Wave 3 closes 1-5 without disturbing the wave-1/2
mirror for built-ins or the dict-backed instances the existing fixtures use.

### The lifetime knot (and how wave 3 ties it)

The hard part of 1-5 is ownership. A faithful body must (a) live as long as the
*instance* (not merely as long as a transient C reference, or fields vanish
between calls), and (b) resolve back to the instance, **without** forming an
ownership cycle (body owns instance owns body => neither is ever freed). Wave 3's
rule, matching PyPy `cpyext`'s borrow model:

- The **native instance owns the body.** `PyInstance` carries a `c_body` pointer;
  the body is freed - exactly once - when the instance's last `Arc` drops (via a
  capi-registered finalizer hook, the same mechanism wave 2 used to register
  `tp_traverse`/`tp_clear`).
- The **body's prefix holds a `Weak<PyInstance>`**, not a strong reference - so
  `clone_object` resolves the pointer back to `Object::Instance` by upgrading the
  weak, and there is no cycle.
- A **C reference is a borrow.** The C `ob_refcnt` rising and falling does not
  free the body; reaching zero merely ends C's *interest*. A process-wide
  strong-holder map represents the one case where C is the *sole* owner (a body
  minted by `PyType_GenericAlloc` and not yet handed to the VM); dropping that
  entry at refcount zero is what lets a never-returned C-built instance die.

The bounded consequence (a deliberate cut, stated in Non-goals): a C reference
does **not** extend the instance's life *beyond* the VM's. This covers the
construct-fill-return and read-during-call patterns that numpy consumers use
(the object is a live Python value while C operates on it); the pathological
"C stashes a raw borrowed pointer and dereferences it after the owning Python
object is gone" case is the same strong-keepalive-across-the-boundary fidelity
wave 2 also deferred.

## CPython reference

Targets **CPython 3.13** as installed on the build host. The layouts wave 3
pins (read out of the host's stock headers):

- **`Include/object.h`** - `PyObject_HEAD` is `ob_refcnt` (offset 0) +
  `ob_type` (offset 8); the first extension field therefore lands at offset 16,
  which is where `tp_basicsize`-block readers begin.
- **`Include/structmember.h`** (now folded into `Include/descrobject.h`) - the
  `PyMemberDef { name; type; offset; flags; doc }` layout and the `T_*` type
  codes (`T_SHORT`=0, `T_INT`=1, `T_LONG`=2, `T_FLOAT`=3, `T_DOUBLE`=4,
  `T_STRING`=5, `T_OBJECT`=6, `T_CHAR`=7, `T_BYTE`=8, `T_UBYTE`=9, `T_USHORT`=10,
  `T_UINT`=11, `T_ULONG`=12, `T_BOOL`=14, `T_OBJECT_EX`=16, `T_LONGLONG`=17,
  `T_ULONGLONG`=18, `T_PYSSIZET`=19). These already exist in WeavePy's
  `getset::member_types`; wave 3 makes them *do* something.
- **`PyType_GenericAlloc`** semantics (`Objects/typeobject.c`): allocate
  `tp_basicsize + nitems * tp_itemsize` bytes, zeroed, refcount 1, `ob_type`
  set and incref'd.
- **The array interface** (numpy's `__array_interface__` version 3 dict:
  `shape`, `typestr`, `data` (a `(addr:int, readonly:bool)` pair), `strides`,
  `version`; and `__array_struct__`, a `PyArrayInterface`-bearing capsule).
- **The array C-API import protocol**: `import_array()` expands to
  `_import_array()`, which calls
  `PyCapsule_Import("numpy._core._multiarray_umath._ARRAY_API", 0)` and stores
  the returned `void **` in the consumer's static `PyArray_API`. WeavePy's
  `PyCapsule_Import` (RFC 0029) already implements the dotted-import +
  name-verification semantics this needs - but the capsule it fetches must first
  *survive* being stored in (and read back out of) the producer module's dict,
  which is the VM-crossing gap WS5 closes.

Explicit non-references (unchanged from waves 1-2): `Py_GIL_DISABLED` layouts,
the `Py_TRACE_REFS` debug head, Windows `.pyd`, and any read of CPython
*internal* (`pycore_*`) structs.

## Current baseline (measured starting point)

- Waves 1-2 green: the `_stockabi` proof (9 cases), the `_stocktype` proof
  (11 cases), the WeavePy-header fixtures (`_smalltest` / `_ndarray` /
  `_numpylike` + the wheel-install round trip), and the full `weavepy-capi`
  suite (88 tests).
- `PyType_GenericAlloc` sizes to `max(tp_basicsize, sizeof(PyObjectBox))` but
  overlays a `PyObjectBox`; `into_owned` mints a fresh box per crossing for
  `Object::Instance`; `tp_members` decodes names but returns `None`.
- `PyCapsule_*` (incl. `PyCapsule_Import` dotted semantics) and the PEP 3118
  buffer protocol are complete *at the C level*; only `datetime.datetime_CAPI` is
  wired into the lazy well-known-capsule path. But a capsule **collapsed to
  `None`** the moment it crossed into the VM (its state lives in the box's
  `user_data`, not in `payload.obj`), so a producer-built capsule stored via
  `PyModule_AddObject` and re-fetched by `PyCapsule_Import` came back a
  non-capsule - the array-C-API round trip never actually worked (the existing
  `_numpylike` test only asserts `_API` is *present*, which `None` passes). WS5
  fixes this.
- No `PyArray_*` / `PyUFunc_*` / `__array_interface__` / `__array_struct__`
  symbol or protocol exists anywhere in the tree.
- All in-tree array fixtures declare `tp_basicsize <= sizeof(PyObject)` (`_stocktype`
  = 16, `_numpylike` = 0) and store per-instance state in `__dict__`, so the
  wave-3 gate (below) leaves every one of them on the existing path.

## Roadmap context

This is wave 3 of the five-wave D1 arc defined in
[RFC 0043 Roadmap](0043-cpython-binary-abi.md). Wave 1 landed the object/type
layouts + mirror; wave 2 the type-suite dispatch + GC; **wave 3 (this RFC)** the
inline instance storage that wave 2 deferred, plus the numpy array C-API surface;
wave 4 builds real numpy from source against the now-faithful host ABI; wave 5 is
pandas / Cython + the wheel matrix.

## Detailed design (wave 3)

Seven workstreams in dependency order.

### WS1 - The gate: which instances get a faithful body (~0.2K LOC)

Not every `Object::Instance` should become a faithful inline body - pure-Python
class instances have no C struct, and the existing dict-backed fixtures must stay
exactly as they are. Wave 3 adds a precise, opt-in discriminator:

> A type is an **inline-instance type** iff it was finalised by `PyType_FromSpec`
> or `PyType_Ready` (i.e. defined by a C extension) **and** declares
> `tp_basicsize > sizeof(PyObject)` (i.e. it has inline fields beyond the head).

A thread-local `INLINE_TYPES: HashSet<*PyTypeObject>` in `crate::types` is
populated at finalisation; `is_inline_instance_type(ty)` is the O(1) query. This
gate has three deliberate properties: it is *additive* (a `PyTypeObject *` that
isn't registered behaves exactly as before); it *excludes* `install_user_type`
(the pure-Python-class fallback path, which is not `FromSpec`/`Ready` and which
sets `tp_basicsize = sizeof(PyObjectBox)` through a different code path); and it
*excludes every current fixture* (all declare `tp_basicsize <= sizeof(PyObject)`),
so the change carries no regression surface for landed tests.

### WS2 - The faithful instance body + its lifetime (~1.2K LOC)

The native instance owns one faithful body for its whole life.

- **`PyInstance` gains `c_body`** (`crates/weavepy-vm`): a `Send + Sync` cell
  holding the body pointer as a `usize` (0 = none). It is excluded from the
  derived `Clone` (a clone is a *distinct* instance that owns no body) via a thin
  `CBody` wrapper whose `Clone` yields the empty state - this keeps the wave-2
  finalizer-resurrection path (which shallow-clones a dying instance) from
  duplicating a pointer and double-freeing.
- **A finalizer hook.** `weavepy-vm` exposes
  `register_instance_body_free(fn(usize))`; `weavepy-capi` registers a function
  that **runs the type's custom `tp_dealloc` first** (for external-resource
  cleanup - e.g. `free(self->data)`), then releases the block. `PyInstance::drop`
  calls it (before the existing `__del__`-resurrection net) when `c_body != 0`,
  so the body and anything its `tp_dealloc` owns are released exactly when the
  instance is collected - the same hook pattern wave 2 used for
  `register_traverse`/`register_clear`. A stock `tp_dealloc`'s own
  `tp_free`/`PyObject_Free`/`PyObject_GC_Del` tail on the body is absorbed (the
  block is VM-owned), so it neither double-frees nor leaks.
- **The body + prefix** (`crate::mirror`). `MirrorPrefix` gains an
  `inst: Option<Weak<PyInstance>>`. A `BodyKind::Instance { size }` body is a
  `tp_basicsize (+ nitems * tp_itemsize)` zeroed block; the prefix's `inst` is a
  `Weak` downgrade of the owning instance and `obj` is `None`. `native_of`
  upgrades the weak to return `Object::Instance(rc)`; `is_mirror` recognises an
  inline-type pointer so `clone_object`/`free_box` route through the mirror path.
- **Get-or-create** (`crate::instance`, new). `instance_body_out(rc, ty, nitems)`
  returns `rc.c_body` (incref'd) if set, else allocates the faithful body, stores
  it on the instance, and downgrades the instance into the prefix. This is the
  single chokepoint that guarantees pointer stability across crossings.
- **C-sole-ownership.** A process-wide `STRONG: HashMap<body_ptr, Rc<PyInstance>>`
  holds the strong reference for an instance minted *by* `PyType_GenericAlloc`
  (where C, not the VM, is the initial owner). On C refcount -> 0, `free_box`
  removes the `STRONG` entry (ending C's ownership) but does **not** deallocate -
  the instance's `Drop` does, if that was the last reference. A VM-owned instance
  borrowed into C has no `STRONG` entry, so its body simply persists across the
  borrow.

### WS3 - `into_owned` / `PyType_GenericAlloc` routing (~0.4K LOC)

The two paths that turn an instance into a C pointer learn the inline route:

- **`into_owned` / `into_owned_with_type`** (`crate::object`): for an
  `Object::Instance` whose type `is_inline_instance_type`, call
  `instance_body_out` instead of boxing; for an `Object::Capsule`, return the
  soul's retained box (WS5). Built-ins still mirror; pure-Python instances and
  modules still box. (Each fork is one `match` arm plus one predicate, so the hot
  path for every other object is unchanged.)
- **`PyType_GenericAlloc`** (`crate::genericalloc`): for an inline type, allocate
  the faithful body (sized `tp_basicsize + nitems*tp_itemsize`), seed a fresh
  `Object::Instance(PyInstance::new(cls))`, register it in `STRONG`, and return
  the body. For every other type the wave-1/2 `PyObjectBox` path is untouched.
- **`free_box`** (`crate::object`): an instance body (recognised by its prefix's
  `inst`) routes to "end C ownership" (drop the `STRONG` entry) rather than
  `free_mirror`'s deallocate-now path.

### WS4 - Real `tp_members` (~0.4K LOC)

`getset::collect_members` stops emitting `None` stubs. Each `PyMemberDef`
becomes an `Object::Property` (so the VM's data-descriptor protocol governs it,
exactly like `collect_getsets`) whose getter/setter:

1. resolve the instance to its stable body via `into_owned` (the same pointer C
   sees), then
2. read/write the C field at `entry.offset` with the width/signedness dictated by
   `entry.ty` - `T_INT`/`T_UINT` (4 bytes), `T_LONG`/`T_LONGLONG`/`T_PYSSIZET`
   (8), `T_FLOAT` (f32) / `T_DOUBLE` (f64), `T_BOOL`/`T_BYTE`/`T_UBYTE` (1),
   `T_SHORT`/`T_USHORT` (2), `T_OBJECT`/`T_OBJECT_EX` (a `*mut PyObject` at the
   offset, bridged via `clone_object` on read and `into_owned` on write),
   honouring `READONLY`.

The result: `obj.size` (Python read), `obj.size = n` (Python write, unless
`READONLY`), and `((ArrayObject *)obj)->size` (C, inlined) are the same eight
bytes.

### WS5 - The numpy array interchange + C-API-capsule surface (~0.8K LOC)

Built on the now-stable body and the capsule machinery:

- **`__array_interface__`** - a producer type whose instances expose the v3
  dict (`{"shape": ..., "typestr": ..., "data": (addr, ro), "strides": ...,
  "version": 3}`) lets any consumer (WeavePy-side or another extension) read the
  buffer without numpy. The `data` address is the *stable body's* inline data
  pointer - only possible because WS2 made it stable.
- **`__array_struct__`** - the same information as a `PyArrayInterface`-bearing
  capsule, for the C-level fast path.
- **The capsule round-trip (the missing soul).** `PyCapsule_*` exists since
  RFC 0029, but a capsule is a legacy `PyObjectBox` whose state (the wrapped
  `void *`, name, …) lives in `user_data` while its `payload.obj` is `None`.
  That made it **collapse to `None`** the moment it crossed into the VM - and a
  capsule's whole job is to live in a place the VM owns: a module dict
  (`module._API`, the `import_array()` idiom) or an attribute
  (`obj.__array_struct__`). So `PyModule_AddObject(m, "_API", capsule)` stored
  `None`, and a later `PyCapsule_Import(...)` fetched a non-capsule and failed -
  the array-C-API pattern was silently broken (the existing `_numpylike` test
  only checked `_API` was *present*, which `None` satisfies). Wave 3 fixes it the
  same way it stabilised instance bodies: the capsule keeps its box, but the VM
  holds an identity-stable `Object::Capsule(Rc<PyCapsuleSoul>)` **soul** that maps
  back to the **same** box. The soul retains one C reference on the box for its
  whole life and hands that same pointer back out on every `into_owned`; a
  `register_capsule_free` hook (the additive-hook pattern again) releases the
  retain when the last soul drops, so the box is freed - running any
  `PyCapsule` destructor - exactly once. `PyCapsuleSoul` stores the box pointer
  as a `usize` (not a pointer), so `Object: Send + Sync` still holds.
- **The array-C-API capsule pattern** - the load-bearing numpy idiom. A producer
  module publishes a `void **` function-pointer table (its "array API") wrapped
  in a named capsule (numpy uses `numpy._core._multiarray_umath._ARRAY_API`); a
  consumer's `import_array()` resolves it through `PyCapsule_Import` and indexes
  the table. Wave 3 proves the *whole loop* hermetically: `_stockarray.c` both
  *publishes* such a capsule and *consumes* it (the `import_array()` shape) to
  read an array's fields through the imported table. (`PyCapsule_Import`'s dotted
  semantics already exist; wave 3 adds the soul that lets the capsule survive the
  module dict, the producer/consumer proof, and the `tp_basicsize`-stable
  instances the table hands around.)

### WS6 - The stock-headers proof fixture + loader path (~1.2K LOC C + tests)

- **`tests/capi_ext/_stockarray.c`** - authored against the **stock CPython
  3.13 headers** (no WeavePy header). It defines `StockArray`, a
  `PyArrayObject`-shaped static type: `PyObject_HEAD` + `int nd` +
  `Py_ssize_t length` + `double *data` + `int typenum`, with
  `tp_basicsize = sizeof(StockArrayObject)`, `tp_members` for `nd`/`length`
  (read-only) and `typenum` (writable), `tp_methods` for constructors/accessors
  (`sum`, `fill`, …), an `__array_interface__` / `__array_struct__` getset pair,
  and an `_ARRAY_API` capsule with a tiny function-pointer table
  (`StockArray_FromLength`, `StockArray_DATA`, `StockArray_LENGTH`). A
  module-level `capi_roundtrip` function exercises the `import_array()` shape:
  `PyCapsule_Import` the table and build a fresh array through it. The inline
  fields are written by `tp_init` straight into the body
  (`a->length = n; a->data = malloc(...)`) and read back in a *later* C call
  (`sum()`) at the same raw offsets, proving the offsets are faithful and the
  body is stable. Crucially the element buffer is **separately `malloc`'d** (not
  an inline tail), so `tp_dealloc`'s `free(a->data)` is a real external-buffer
  free that the body-free hook must run on collection - which it does (see the
  drawback below, now resolved for the synchronous case).
- **`build.rs`** compiles it with the stock include dir (the existing
  `stock_python_include()` probe), emitting `WEAVEPY_CAPI_STOCKARRAY_EXTENSION`;
  absent CPython 3.13 the fixture is skipped (a bare CI host still passes), and a
  `rerun-if-changed` keeps it rebuilt.
- **`force_link_table`** registers any newly `#[no_mangle]` symbol the fixture
  resolves (so a missing symbol is a loud link failure, never a silent "false
  pass").

### WS7 - Integration tests + measured baseline (~0.5K LOC)

`crates/weavepy-capi/tests/capi_stockarray.rs` `dlopen`s the fixture and asserts:
inline field reads (a value set from Python via a member is read by C at the raw
offset and vice-versa), `tp_members` round trips (read + write + `READONLY`
rejection), pointer stability (two crossings of the same instance see one
address; a field written in call 1 is read in call 2), the buffer export, the
`__array_interface__` dict shape, and the array-C-API capsule round trip
(publish + `import_array()`-style consume). Gated on the CPython-3.13 env var so a
bare host skips cleanly. The CPython `Lib/test` sweep is unaffected (wave 3 is
C-API-only infrastructure; no `expectations.toml` row flips).

## Measured targets

The commit-acceptance bar for wave 3:

- A stock-CPython-3.13-headers extension (`_stockarray`) defining a
  `PyArrayObject`-shaped type with **inline `tp_basicsize` fields** loads via
  `dlopen` and runs: C reads/writes those fields at their raw offsets, the same
  bytes Python sees through `tp_members`.
- The same instance presents a **stable pointer** across crossings: a field
  written by C in one call is observed in the next.
- The numpy array interchange (`__array_interface__` / `__array_struct__`) and
  the array-C-API capsule pattern (publish + `import_array()`-style consume) work
  end-to-end.
- The wave-1/2 fixtures (`_stockabi`, `_stocktype`, `_smalltest`, `_ndarray`,
  `_numpylike` + wheel install) and the whole `weavepy-capi` suite stay green
  through the instance-representation change.
- `cargo build --workspace`, `cargo fmt --check`, and
  `cargo clippy --workspace --all-targets -- -D warnings` are green; the regrtest
  sweep stays behaviourally `--check` clean on the release binary (no output/
  status regressions; any parallel-run wall-clock timeouts must resolve to their
  expected status when re-run serially).

## Measured outcome

Landed as designed. The hermetic proof fixture `_stockarray` (stock CPython 3.13
headers, no WeavePy header) passes **11/11** in `capi_stockarray.rs`:

- `inline_storage_persists_across_calls`, `data_pointer_is_stable`,
  `fill_then_sum` - `tp_init` writes `nd`/`length`/`data`/`typenum` straight into
  the body; a *later* C call (`sum`, `fill`) reads them back at the same raw
  `tp_basicsize` offsets through the **same** pointer.
- `members_read_inline_fields`, `member_write_roundtrips` - `tp_members` project
  `nd`/`length` (read-only) and `typenum` (writable) at their `offsetof`; the
  Python view and the C inlined read/write are the same bytes; a write to a
  `READONLY` member raises.
- `array_interface`, `array_struct_capsule` - the `__array_interface__` v3 dict
  and the `PyArrayInterface`-bearing `__array_struct__` capsule both expose the
  stable inline buffer.
- `import_array_capsule_roundtrip` - `capi_roundtrip(4)` runs the full
  `import_array()` loop in C (`PyCapsule_Import("_stockarray._ARRAY_API")` -> a
  `void **` table -> `StockArray_FromLength`) and the resulting array's
  `sum() == 6.0`. **This is the test that the capsule-soul (WS5) makes possible;
  before it, the `_ARRAY_API` capsule collapsed to `None` in the module dict.**
- `dealloc_frees_buffer` - dropping the sole reference collects the native
  instance, whose free hook runs the extension `tp_dealloc` (`free(self->data)`,
  a real *external* buffer) and absorbs its `PyObject_Free` tail; proven race-free
  via a monotonic `dealloc_count()` (the `.so`'s counters are shared across the
  parallel test process).

No regressions: the entire `weavepy-capi` suite is green - **99 tests, 0 failed**
across the lib unit tests (23) and every fixture binary (`capi_buffer` 10,
`capi_loader` 6, `capi_ndarray` 14, `capi_numpylike` 14, `capi_stockabi` 9,
`capi_stockarray` 11, `capi_stocktype` 11, `capi_wheel_endtoend` 1). The wave-3
instance-representation fork and the `Object::Capsule` soul left the wave-1/2
fixtures and the wheel install/import round-trip untouched.

Hygiene: `cargo build --workspace --all-targets`, `cargo fmt --all`, and
`cargo clippy --workspace --all-targets` are all clean. Adding `Object::Capsule`
to the VM enum surfaced exactly **six** exhaustive `match` sites (`class_of`,
`object_identity`, `is_truthy`, `type_name`, `repr`, `id_of`) - all now handled;
no other workspace crate matches `Object` exhaustively.

The curated regrtest conformance sweep shows **zero behavioural regressions**
from wave 3. On the **release** binary (the build `expectations.toml`'s per-test
wall budgets are calibrated for) the 227-row sweep grades pass 173 / fail 32
(every `fail` is a pre-existing expected `fail`) / skip 13. A `--jobs 6` parallel
run on a loaded host flagged 9 rows, but all 9 are wall-clock contention
artifacts, not output divergences: re-run **serially** (`--jobs 1`, same release
binary) every one resolves to its expected status - 8 compute/IO/multiprocessing
-heavy tests (`test_json` 27.7s, `test_queue`, `test_statistics`, `test_zipfile`,
`test_set` 63s, `test_tarfile`, `test_multiprocessing_main_handling`,
`test_concurrent_futures` - the last a benign `resource_tracker` leaked-semaphore
*shutdown warning* under load) grade `pass`, and `test_pathlib` grades its
expected `fail`. (Grading the **debug** binary instead inflates this to ~41
spurious timeouts purely because debug Rust runs the heavy numeric/IO suites
10-30x slower than the release-calibrated budgets - e.g. `test_math` is ~13s
release vs >60s debug; not a behaviour change.) None of these tests reach the
C-API instance/capsule path: wave 3 is C-API-only infrastructure that no
pure-Python path exercises.

## Non-goals / Drawbacks

- **Real numpy does not import in wave 3.** Wave 3 lands the *foundation* numpy
  needs (faithful instance layout, `tp_members`, the array interchange + C-API
  capsule pattern) and proves it hermetically; building numpy from source against
  the host ABI and gating CI on `import numpy; numpy.zeros((3,3)) @ ...` is
  wave 4. The `_stockarray` fixture is numpy-*shaped*, not numpy.
- **The full ufunc-loop registration machinery is wave 4.** `PyUFunc_FromFuncAndData`
  with its inner-loop dispatch, type resolution, and casting tables is large and
  numpy-specific; wave 3 proves the array-C-API *capsule* mechanism a ufunc
  registration would ride on, not the loop machinery itself.
- **External `data`-buffer `tp_dealloc`-on-finalize works for the synchronous
  case; deep re-entrancy is bounded.** The body-free hook runs the type's custom
  `tp_dealloc` before releasing the block, so a type that frees a
  *separately-`malloc`'d* `data` block (real numpy's shape) is reclaimed
  correctly - `_stockarray` does exactly this (`free(a->data)` in `tp_dealloc`)
  and the `stockarray_dealloc_frees_buffer` test proves the buffer is freed on
  collection. What remains a wave-4 concern is the *exotic re-entrancy* hazard: a
  `tp_dealloc` that itself triggers further collection (the array-object analogue
  of the wave-2 `tp_traverse`-during-collection case). The synchronous
  construct/use/drop lifecycle numpy consumers exercise is covered.
- **A C reference does not outlive the owning Python object.** The borrow model
  (WS2) keeps a body alive for the instance's life, not for a raw C pointer
  stashed past it - the same strong-keepalive-across-the-boundary fidelity wave 2
  deferred. Synchronous construct/fill/read patterns (what numpy consumers use)
  are covered.
- **The inline gate is opt-in by `tp_basicsize`.** A C-extension type that wants a
  faithful instance body must declare `tp_basicsize > sizeof(PyObject)` - which
  every real field-bearing type does. Types that store everything in `__dict__`
  (the `_core_addr` fixtures) keep the wave-1/2 box, by design.

## Alternatives

1. **Keep the `_core_addr` side-allocation idiom (status quo).** Works for a
   hand-written fixture but is exactly what a stock wheel does *not* do - numpy
   reads `arr->data` inlined, never through a dict. Rejected: it is the gap.
2. **Lay the faithful fields *after* a `PyObjectBox` (extend the existing box).**
   Tempting (no new allocation path), but the fields would then sit at
   `sizeof(PyObjectBox) + offset`, not `PyObject_HEAD + offset`, so every stock
   inlined accessor would read the wrong bytes. The body must begin with a real
   `PyObject_HEAD` and nothing else before the fields. Rejected.
3. **A process-wide `Object`->pointer cache instead of owning the body on the
   instance.** A cache keyed by `Rc::as_ptr` gives pointer stability, but without
   the instance owning (and freeing) the body the lifetime is unanchored - either
   it leaks (cache never evicts) or it frees on C refcount zero (fields vanish
   between calls). Anchoring ownership on `PyInstance` with a `Weak` back-ref is
   what makes the lifetime correct and cycle-free.
4. **Strong `Object::Instance` in the prefix (like built-in mirrors).** That is
   the natural extension of wave 1, but for an instance body it forms an
   ownership cycle (body -> instance -> `c_body` -> body) that never collects.
   The `Weak` prefix + instance-owned body is the cycle-free dual.

## Prior art

- **PyPy `cpyext`.** Its `W_Root <-> py_obj` link - the app object owns the C
  mirror, the mirror holds a borrow back, the link is broken at the right
  refcount - is the direct model for wave 3's instance-owned body + `Weak`
  prefix + `STRONG`-map borrow accounting.
- **GraalPy's native C-API.** Maintains native mirror structs for managed objects
  with a managed back-reference and frees them on managed collection; validates
  the "instance owns its faithful body" lifetime at scale.
- **numpy's `Include/numpy/arrayobject.h` / `ndarraytypes.h`** - the authoritative
  `PyArrayObject` layout and the `_ARRAY_API`/`import_array()` capsule protocol
  the fixture is shaped to.
- **RFC 0043 (wave 1)** the mirror + negative-offset prefix wave 3 extends;
  **RFC 0044 (wave 2)** the `register_traverse`/`register_clear` hook pattern
  wave 3's body-free hook mirrors; **RFC 0029** the capsule + buffer surface
  wave 3's interchange stands on.

## Future work

- Wave 4: build real numpy from source; the ufunc-loop machinery; the external
  `data`-buffer `tp_dealloc`-on-finalize; the complete private
  `_multiarray_umath` symbol tail.
- Wave 5: pandas / Cython + the manylinux/macOS wheel matrix.
- Mirror/instance-body performance (arena allocation, fewer crossings).
- Strong-keepalive-across-the-boundary fidelity (a C reference that outlives the
  owning Python object), if a real wheel is found to need it.
