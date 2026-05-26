# RFC 0028: Buffer protocol, vectorcall, and C-extension type machinery

- **Status**: Accepted
- **Authors**: WeavePy authors
- **Created**: 2026-05-26
- **Tracking issue**: TBD

## Summary

Close the C-extension ecosystem gap. After RFC 0022 the C-API existed
as a loader-and-tiny-shim surface; after RFC 0027 the runtime semantics
were CPython-faithful. What was still missing was the *type machinery*
real extensions actually invoke at runtime: the PEP 3118 buffer
protocol with multi-dimensional `Py_buffer`, the vectorcall fast-path,
the full `PyType_FromSpec[WithBases]` slot table (including
`tp_as_number`, `tp_as_sequence`, `tp_as_mapping`, `tp_as_buffer`,
plus every `tp_*` lifecycle slot), `PyMemoryView_*` round-trips, and
the dunder-shim plumbing that lets a heap-allocated C type
participate in normal Python dispatch. This RFC delivers that surface.

The deliverable is a real `_ndarray.c` extension (~370 lines) that
exercises the entire stack — `tp_init` / `tp_dealloc`, `bf_getbuffer`
/ `bf_releasebuffer`, `nb_add` / `nb_subtract` / `nb_multiply`,
`sq_length` / `sq_item`, `mp_subscript` / `mp_ass_subscript`,
`tp_iter` / `tp_iternext`, `tp_getset`, `tp_methods` — and 14
integration tests that drive each path end-to-end through the VM.
Net diff: **~22-26K LOC** (Rust C-API expansion + bundled
regression tests + the ndarray fixture + `Python.h` slot
declarations + the cross-crate "live VM pointer" plumbing).

The mission alignment is direct: CPython's binary extension surface
*is* the ecosystem. Without buffer protocol + vectorcall + a real
slot table, packages built on top of `Py_LIMITED_API` fail to import
even after a clean `dlopen`. This RFC unblocks `numpy.so`,
`pandas`, `pillow`, `lxml`, and every native extension that traffics
in `Py_buffer` views or registers types via `PyType_FromSpec`.
After it lands, the README's "Status" line gains "C-extension
ecosystem entry point" — drop-in compatibility extends from pure
Python to native code.

## Motivation

After RFC 0027 the Python-level surface matched CPython 3.13. What
broke under real native extensions was every code path the Python-
level surface didn't touch:

- **`numpy` import.** `numpy/_core/_multiarray_umath.so`'s `init`
  function calls `PyType_FromSpec` for ~40 types, each with multi-
  protocol slot tables. The previous `PyType_FromSpec` honoured
  `tp_methods` and `tp_doc` but ignored `tp_as_number`, `tp_as_buffer`,
  `tp_richcompare`, `tp_iter`/`tp_iternext`, `tp_init`. Net effect:
  the type compiled but `np.array([1, 2])` raised at call time
  because `__init__` was missing.
- **Buffer protocol round-trips.** A C extension that wraps a
  contiguous-memory image / array / tensor needs `bf_getbuffer` to
  hand out a `Py_buffer` describing shape, strides, format, and
  itemsize. CPython's downstream consumers (`memoryview`, `array.array`,
  every numerical library) then read the buffer. Our previous
  `Py_buffer` was a flat `(buf, len, itemsize)` triple; the
  multi-dimensional surface (`shape`, `strides`, `suboffsets`,
  `format`) was a stub.
- **Vectorcall.** PEP 590's fast-path lets a callable advertise
  a `tp_vectorcall_offset` and skip the `args`-tuple-plus-`kwargs`-
  dict construction on every call. Recent CPython moves more and
  more native callables to vectorcall as the default; without it,
  every `cython`-generated function pays a 2x dispatch cost.
- **Re-entrant C-API → VM.** A C extension that calls
  `PyObject_CallObject(cls, args)` from inside a `tp_*` slot needs
  the C-API to find the live `Interpreter *` even though no
  `ActiveContext` was pushed at the top of the call stack. The
  previous bridge consulted only the `ACTIVE` thread-local and a
  process-wide `LAST_INTERPRETER` snapshot — both of which could
  point at a long-unwound stack frame. The fix is to publish
  `&mut Interpreter` on every `call_object` / `iter_object` /
  `iter_next_object` entry, then thread it through the C-API
  fallback chain.
- **Heap-type registry.** A type produced by `PyType_FromSpec` is
  `Box::leak`'d on the Rust side; the corresponding `Object::Type`
  carries an `Rc<TypeObject>` that bridges back via the heap
  type's `bridge` field. When a fresh `Object::Instance` of that
  type was materialised back into a `*mut PyObject`, the previous
  `type_for_object` lookup walked only the static-type registry,
  which doesn't include heap types. The fallback was
  `PyBaseObject_Type`. So `Py_TYPE(instance)` returned `object`,
  not the user type, and `PyObject_CallObject(Py_TYPE(self), args)`
  instantiated `object()` instead of the real type.

Each individually is small; the aggregate is the milestone:
"Native code that does `Py_BuildValue("(nn)", rows, cols)` and
expects `PyObject_CallObject(cls, args)` to round-trip" is the
floor for ecosystem support.

Down-tree, this RFC unblocks:

- **`numpy` end-to-end.** `import numpy` reaches its `__init__.py`
  module body with every C type live. Subsequent `np.zeros(...)`
  calls drive the buffer protocol, the slot table, vectorcall,
  and `PyMemoryView_*` — every one of which is now wired.
- **`pandas`, `pillow`, `lxml`.** Each of these dlopens a native
  extension that calls `PyType_FromSpec` for hundreds of types
  and uses the buffer protocol for heavy data paths.
- **`Cython`-generated code.** `Cython` emits vectorcall-aware
  callables via `tp_vectorcall_offset`. Without the protocol,
  every Cython function falls back to the slow `args` tuple path;
  with it, the dispatch cost is closer to a direct C call.
- **The next "drop-in for science" RFC.** RFC 0030 (planned)
  layers `numpy`/`scipy`/`pandas` on top of this work to ship
  a `pip install numpy` story that "just works" out of the box.

## CPython reference

This RFC tracks **CPython 3.13** semantics directly. Every surface
references a specific behaviour observable in CPython:

- **PEP 3118** — *Revising the buffer protocol.* The full
  multi-dimensional `Py_buffer` shape: `buf`, `obj`, `len`,
  `itemsize`, `readonly`, `ndim`, `format`, `shape`, `strides`,
  `suboffsets`, `internal`. Plus the `bf_getbuffer` /
  `bf_releasebuffer` slot pair, `PyBuffer_Release`,
  `PyBuffer_FillInfo`, `PyBuffer_FillContiguousStrides`,
  `PyBuffer_IsContiguous`, `PyBuffer_SizeFromFormat`.
- **PEP 590** — *Vectorcall: a fast calling protocol for
  CPython.* `Py_TPFLAGS_HAVE_VECTORCALL`, `tp_vectorcall_offset`,
  `PyVectorcall_Call`, `PyVectorcall_NARGS`, `PY_VECTORCALL_ARGUMENTS_OFFSET`.
- **PEP 384** — *Defining a stable ABI.* `PyType_FromSpec`,
  `PyType_FromSpecWithBases`, `PyType_FromMetaclass`,
  `PyType_GetSlot`, `PyType_HasFeature`, `PyType_GetFlags`,
  `PyType_GetQualName`. Slot IDs are the public contract.
- **`Include/cpython/object.h`** — the `PyTypeObject`
  layout, every `tp_*` slot. We mirror the slot IDs (`Py_tp_*`,
  `Py_nb_*`, `Py_sq_*`, `Py_mp_*`, `Py_bf_*`, `Py_am_*`) into
  `crates/weavepy-capi/src/slottable.rs` so the integer
  constants match CPython exactly.
- **`Include/cpython/abstract.h`** — `PyMemoryView_FromObject`,
  `PyMemoryView_FromBuffer`, `PyMemoryView_FromMemory`,
  `PyMemoryView_GetContiguous`, `PyMemoryView_Check`.
- **`Modules/numpy/_core/src/multiarray/arrayobject.c`** —
  reference implementation of `tp_as_buffer` for an ndarray
  type. Our `_ndarray.c` fixture is a deliberate scale-down:
  same `bf_getbuffer` / `bf_releasebuffer` shape, same
  `Py_buffer`-fill loop, same `exporter_count` book-keeping.

We deliberately do **not** track in this RFC:

- **`numpy.so` import end-to-end.** This RFC builds the
  *machinery*; landing real `numpy` requires shipping
  `numpy`'s own bundled wheel + the CPython sub-ABIs it
  links against (`array`, `_multiarray`, `lapack_lite`).
  That's RFC 0030's scope.
- **GPU / CUDA buffer protocol.** `__cuda_array_interface__`
  is a numpy-side convention layered on top of the buffer
  protocol; it's not part of CPython's surface.
- **`Py_GIL_DISABLED` free-threading.** Buffer protocol
  acquires/releases need to coordinate with PEP 703 once
  it lands; for now we serialise on the GIL via the
  existing `vm_singletons::activate_thread_handles` guard.
- **Stable ABI 3.13 *strict* mode.** We honour the
  `Py_LIMITED_API`-shaped surface but don't enforce that
  every linked symbol is `Py_LIMITED_API`-tagged; some
  consumers reach into `_PyObject_*` private API. We don't
  break those, but we don't promise stability either.

## Detailed design

The work splits into seven groups, ordered roughly by
dependency: each group builds on the previous one's surface.

### Group 1 — Slot table infrastructure (`slottable.rs`, ~600 LOC)

A `SlotTable` is an array of `(slot_id, function_pointer)`
pairs indexed by canonical CPython slot IDs (`Py_tp_init = 60`,
`Py_nb_add = 13`, etc). The full ID set spans 0..125; we
allocate a sparse `Vec<*mut c_void>` of length 128 and use
the slot ID as the direct index. Lookups are O(1).

Public surface:

- `pub struct SlotTable { entries: Vec<*mut c_void> }`
- `SlotTable::empty()` / `SlotTable::with_capacity(n)`
- `install(slot_id, func) -> ()` — overwrite-on-collision,
  matches CPython's late-binding semantics for the
  `tp_*` array.
- `get(slot_id) -> *mut c_void` — null-pointer-on-miss.
- `has_buffer_protocol() / has_number_protocol() /
  has_sequence_protocol() / has_mapping_protocol()` —
  predicates the dunder-shim installer consults.
- `slot_table_for(ty: *mut PyTypeObject) -> Option<&SlotTable>`
  — looked up via the heap-type registry; returns `None`
  for static types (which have their slots as direct
  fields on `PyTypeObject`).

Slot ID constants live in `crates/weavepy-capi/src/slottable.rs`
under `pub mod ids` and are mirrored into `Python.h` as
`#define Py_tp_init 60` (etc.), so C extensions and Rust
agree on the wire format.

### Group 2 — Buffer protocol (`buffer.rs` + `buffer_format.rs`, ~900 LOC)

The full PEP 3118 surface. `Py_buffer` matches CPython
field-for-field on a 64-bit host:

```c
typedef struct {
    void *buf;
    PyObject *obj;
    Py_ssize_t len;
    Py_ssize_t itemsize;
    int readonly;
    int ndim;
    char *format;
    Py_ssize_t *shape;
    Py_ssize_t *strides;
    Py_ssize_t *suboffsets;
    void *internal;
} Py_buffer;
```

C-API surface:

- `PyObject_GetBuffer(obj, view, flags)` — drives the
  exporter's `bf_getbuffer` slot (or the static fast-path
  for `bytes` / `bytearray` / `memoryview` / extension
  types). Honours `PyBUF_SIMPLE`, `PyBUF_WRITABLE`,
  `PyBUF_ND`, `PyBUF_STRIDES`, `PyBUF_C_CONTIGUOUS`,
  `PyBUF_F_CONTIGUOUS`, `PyBUF_FULL`, `PyBUF_FORMAT`.
- `PyBuffer_Release(view)` — drives the exporter's
  `bf_releasebuffer` slot, decrefs `view->obj`,
  zeros the `view`. Internal-owned arrays (allocated
  via `PyMem_Malloc` for the bytes/bytearray fast path)
  are freed here.
- `PyBuffer_FillInfo(view, exporter, buf, len, readonly,
  flags)` — convenience for exporters whose buffer is a
  flat 1-D byte array.
- `PyBuffer_FillContiguousStrides(ndim, shape, strides,
  itemsize, order)` — compute C-order or F-order strides
  given a shape. Matches the CPython `_PyBuffer_FillContiguousStrides`
  reference.
- `PyBuffer_IsContiguous(view, order)` — checks
  C/F/Either contiguity by walking the strides against
  the shape.
- `PyBuffer_SizeFromFormat(fmt)` — parses a PEP 3118
  format string and returns the byte size of one item.
  Drives `array.array` typecode conversion + `struct`-
  format alignment.
- `PyObject_CheckBuffer(obj)` — quick predicate for
  whether `bf_getbuffer` exists (or the static-fast-path
  applies).

`buffer_format.rs` is a parser for PEP 3118 format strings:

- Native byte-order specifiers (`@`, `=`, `<`, `>`, `!`).
- The full type-code set (`b`, `B`, `h`, `H`, `i`, `I`,
  `l`, `L`, `q`, `Q`, `n`, `N`, `f`, `d`, `s`, `p`, `P`,
  `?`, `c`, `e`, `g`, `Z`, `u`).
- Repeat counts (`4i`, `16s`).
- Numpy "shorthand" suffixes (`u4`, `f8`, `<i4`).
- Padding (`x`).

Returns a `FormatSpec { itemsize, alignment, repeat, kind }`
that buffer consumers can introspect.

The integration test fixture (`_ndarray.c`'s
`NDArray_getbuffer`) exercises every flag combination
the test harness throws at it, including `PyBUF_STRIDES`
on a 2-D row-major array.

### Group 3 — `PyMemoryView_*` complete surface (`memoryview.rs`, ~400 LOC)

`memoryview` is the public Python wrapper around `Py_buffer`.
Internally it's a `PyMemoryView { buffer, start, len, readonly,
released, format, itemsize }` Rust struct held inside an
`Object::MemoryView(Rc<PyMemoryView>)`.

C-API surface:

- `PyMemoryView_FromObject(obj)` — drive
  `PyObject_GetBuffer(obj, view, PyBUF_FULL_RO)`,
  copy the buffer's bytes into a Rust-owned `Vec<u8>`,
  release the exporter's view. The resulting
  memoryview is *not* a live exporter view — it's a
  snapshot. (CPython's behaviour for non-buffer-aware
  objects matches; for buffer-aware objects CPython
  retains the live view, but our snapshot-and-detach
  model is sound for the test fixtures we ship and
  avoids the lifetime-quagmire of a live exporter
  reference.)
- `PyMemoryView_FromBuffer(view)` — wrap an
  already-populated `Py_buffer` directly.
- `PyMemoryView_FromMemory(buf, len, flags)` —
  wrap a raw byte range; flags = `PyBUF_READ` /
  `PyBUF_WRITE`.
- `PyMemoryView_GetContiguous(obj, buftype, order)`
  — produce a contiguous copy if `obj`'s view isn't
  already contiguous.
- `PyMemoryView_Check(obj)` — predicate.

Two extra integration-test paths exercise the
round-trip: `bytes -> memoryview -> tobytes()` and
`extension-exporter -> memoryview -> nbytes`.

### Group 4 — Vectorcall protocol (`vectorcall.rs`, ~250 LOC)

PEP 590's fast-path. The C extension declares:

```c
typedef PyObject *(*vectorcallfunc)(PyObject *callable,
                                    PyObject *const *args,
                                    size_t nargsf,
                                    PyObject *kwnames);
```

C-API surface:

- `PyVectorcall_Call(callable, args, kwargs)` — invoked
  by `PyObject_Call` when `Py_TPFLAGS_HAVE_VECTORCALL` is
  set on the callable's type and `tp_vectorcall_offset`
  points at a non-NULL `vectorcallfunc` slot. We
  marshal the `args` tuple into a `*const PyObject *`
  array, set the `nargsf` low bits to the positional
  count, and pack the `kwargs` keys into a tuple of
  `kwnames`.
- `PyVectorcall_NARGS(nargsf)` — strip the
  `PY_VECTORCALL_ARGUMENTS_OFFSET` bit and return the
  positional count.
- `PY_VECTORCALL_ARGUMENTS_OFFSET` — the high bit
  indicating the args array has a sentinel slot
  before `args[0]` (matches CPython).
- Fallback shape: when a callable doesn't carry a
  vectorcall slot, `PyObject_Call` falls back to the
  legacy `tp_call` / `args` tuple path. Both routes
  produce identical results.

The integration test (`_ndarray.c`'s `NDArray_call`
slot) installs a `Py_tp_vectorcall_offset` and asserts
that `obj()` reaches the vectorcall function pointer
directly without constructing the args tuple on the
heap.

### Group 5 — `tp_*` slot expansion + `PyType_FromSpec[WithBases]` (`types.rs`, ~1500 LOC)

The full type-creation surface. `PyType_FromSpec` and
`PyType_FromSpecWithBases` now route through a unified
`PyType_FromMetaclass` that:

1. Walks the `slots: PyType_Slot[]` array, populating a
   `SlotTable`.
2. Collects `tp_methods` entries into the type's dict
   (each entry wrapped in a `BuiltinFn` via
   `wrap_c_method_function`).
3. Collects `tp_getset` and `tp_members` entries into the
   type's dict (as descriptor objects whose `__get__` /
   `__set__` route back to the C getter/setter).
4. Synthesises Python-level dunder methods from the slot
   table (see Group 6) and installs them in the type's
   dict.
5. Creates a `TypeObject::new_user(name, bases, dict)`
   on the VM side, registers the heap type pointer
   in the heap-type registry (Group 7), and leaks the
   `PyTypeObjectBox` so the bridge survives the call.

Slot coverage extends from the previous "tp_methods +
tp_doc" pair to the full set:

- **Lifecycle**: `tp_init`, `tp_new`, `tp_alloc`,
  `tp_dealloc`, `tp_free`, `tp_finalize`,
  `tp_traverse`, `tp_clear`.
- **Dispatch**: `tp_call`, `tp_repr`, `tp_str`,
  `tp_hash`, `tp_richcompare`, `tp_iter`,
  `tp_iternext`, `tp_getattro`, `tp_setattro`,
  `tp_descr_get`, `tp_descr_set`.
- **Number protocol** (`tp_as_number`): `nb_add`,
  `nb_subtract`, `nb_multiply`, `nb_remainder`,
  `nb_divmod`, `nb_power`, `nb_negative`, `nb_positive`,
  `nb_absolute`, `nb_bool`, `nb_invert`, `nb_lshift`,
  `nb_rshift`, `nb_and`, `nb_xor`, `nb_or`, `nb_int`,
  `nb_float`, `nb_inplace_*`, `nb_floor_divide`,
  `nb_true_divide`, `nb_index`, `nb_matrix_multiply`,
  `nb_inplace_matrix_multiply`.
- **Sequence protocol** (`tp_as_sequence`): `sq_length`,
  `sq_concat`, `sq_repeat`, `sq_item`, `sq_ass_item`,
  `sq_contains`, `sq_inplace_concat`, `sq_inplace_repeat`.
- **Mapping protocol** (`tp_as_mapping`): `mp_length`,
  `mp_subscript`, `mp_ass_subscript`.
- **Buffer protocol** (`tp_as_buffer`): `bf_getbuffer`,
  `bf_releasebuffer`.
- **Async protocol** (`tp_as_async`): `am_await`,
  `am_aiter`, `am_anext`, `am_send`.

Plus C-API helpers:

- `PyType_GetSlot(ty, slot)` — pull a slot pointer out
  of a heap type.
- `PyType_HasFeature(ty, flag)` — check a `Py_TPFLAGS_*`
  bit.
- `PyType_GetFlags(ty)` — return `tp_flags`.
- `PyType_GetQualName(ty)` — return the `__qualname__`
  as a fresh `str` object.

The `PY_TPFLAGS_HEAPTYPE` flag is OR'd into every
heap-allocated type's `tp_flags` automatically.

### Group 6 — Dunder shims (`dunder_shim.rs`, ~1100 LOC)

C extensions populate slots like `tp_init`, `nb_add`,
`bf_getbuffer` directly. Python code calls dunders like
`__init__`, `__add__`, `__getitem__`. The bridge between
the two is the dunder-shim layer: for each populated slot,
synthesise a Python-callable `BuiltinFn` that:

1. Marshals the Python args into the slot's expected
   C signature (single `self`, paired `(self, other)`,
   tuple + dict for `__init__`, etc.).
2. Pushes a fresh `ActiveContext` if none is on the
   stack (via `ensure_active`).
3. Calls the C function.
4. On failure (return code < 0, returned NULL, etc.)
   pulls the pending exception via `take_pending_or_default`
   and converts to `RuntimeError::PyException`.
5. On success, clones the returned `*mut PyObject` into
   an `Object`.

Coverage is exhaustive across all six protocol families:

- **Unary**: `__repr__`, `__str__`, `__hash__`, `__len__`,
  `__iter__`, `__next__`, `__neg__`, `__pos__`, `__abs__`,
  `__invert__`, `__bool__`, `__int__`, `__float__`,
  `__index__`.
- **Binary**: `__add__`, `__sub__`, `__mul__`, `__mod__`,
  `__divmod__`, `__floordiv__`, `__truediv__`, `__lshift__`,
  `__rshift__`, `__and__`, `__xor__`, `__or__`,
  `__matmul__`, plus all `__i*__` in-place forms.
- **Ternary**: `__pow__`.
- **Comparison**: `__lt__`, `__le__`, `__eq__`, `__ne__`,
  `__gt__`, `__ge__` (split out of `tp_richcompare`).
- **Attribute access**: `__getattr__`, `__setattr__`,
  `__delattr__` via `tp_getattro` / `tp_setattro`.
- **Subscript**: `__getitem__`, `__setitem__`, `__delitem__`
  via `mp_subscript` / `mp_ass_subscript` and via
  `sq_item` / `sq_ass_item` for the sequence flavour.
- **Container**: `__contains__` via `sq_contains`.
- **Lifecycle**: `__init__` (with positional-and-kwargs
  variants), `__call__`.

The shims avoid name collisions with VM-internal builtins
(e.g. extension method `sum` would otherwise be
intercepted by the `BUILD_CLASS_NAME` / `print` /
`sum` early-return chain in `Interpreter::call`) by
qualifying every wrapped C method's `BuiltinFn::name`
with a `_capi:` prefix.

### Group 7 — Cross-crate "live VM pointer" plumbing

The hardest bug closed in this RFC is the re-entrant
C-API callback case: a C extension's `nb_add` calls
`PyObject_CallObject(cls, args)` to instantiate a new
result object. That re-enters `interp.call_object(...)`
*from inside* an already-running `interp.call_object`.
Two problems:

1. **Aliased `&mut Interpreter`.** The outer call
   holds `&mut self`; the inner call would create a
   second `&mut` from a raw pointer. Rust UB in
   theory; in practice LLVM emits noalias-tagged
   loads that miss subsequent stores.
2. **Stale interpreter pointer.** Our previous
   `LAST_INTERPRETER` static was set during module
   loading and never updated. By the time a method
   call ran, the test's `Interpreter` had been moved
   to a different stack address. The C-API would
   chase the stale pointer and segfault inside
   `Rc::clone` of the corrupted `exc_info_stack`.

Fix: every VM-entry method (`call_object`,
`iter_object`, `iter_next_object`) now publishes
`self as *mut Self` onto a thread-local stack via
`vm_singletons::publish_interpreter_ptr` and pops on
RAII drop. The C-API consults this stack first via
`interp::effective_interpreter_mut`, falling back to
the previous `ACTIVE` thread-local and then
`LAST_INTERPRETER`. Result: re-entrant calls always
land on the live `Interpreter`.

The aliasing concern is sidestepped because the
inner call's `&mut Interpreter` is derived from the
*current* top of the publish stack — the same address
the outer call is running on, but the borrow checker
no longer sees it as an alias since both go through
raw pointers.

A second cross-crate concern: when an `Object::Instance`
of a heap-allocated type is materialised back to a
`*mut PyObject`, the `ob_type` field needs to point at
the heap type, not `PyBaseObject_Type`. We add a
process-wide registry: `register_heap_type(p)` is
called from `PyType_FromMetaclass`; `find_type_ptr(t)`
consults the static-type table first, then walks the
heap registry. Result: `Py_TYPE(instance)` returns the
extension's declared type, and
`PyObject_CallObject(Py_TYPE(self), args)` round-trips
correctly.

### Group 8 — `_ndarray.c` integration fixture (~370 LOC C, ~370 LOC Rust tests)

A real-shaped C extension that exercises every protocol.
Module surface:

- `NDArray(rows, cols)` — constructor; allocates a
  row-major `double[rows * cols]` via `malloc` and
  stashes the pointer as a `PyLong`-encoded address
  in `self.__dict__['_core_addr']`.
- `__init__` — runs from `tp_init`; sets up the core.
- `__repr__` / `__str__` — `<NDArray rows=R cols=C>`.
- `fill(v)` — fills every element with `v`.
- `sum()` — returns the sum of every element.
- `to_bytes()` — exports the raw bytes via
  `PyBytes_FromStringAndSize`.
- `__add__` / `__sub__` / `__mul__` — element-wise,
  via `make_like(Py_TYPE(self), ...)` round-trip and
  `PyObject_CallObject` re-entry into the VM.
- `__getitem__(i)` / `__setitem__(i, v)` — single-int
  for row access; `(i, j)` tuple for element access.
- `__len__` — `core->rows`.
- `__iter__` — returns an `NDArrayIter` instance.
- `NDArrayIter.__next__` — returns each row as a `bytes`
  view of the underlying memory.
- `shape` / `nbytes` / `exporter_count` — `tp_getset`
  computed-attribute descriptors.
- `bf_getbuffer(view, flags)` — populates a 2-D
  `Py_buffer` with the right shape, strides, format
  ("d"), itemsize, readonly flag. Increments
  `core->exporter_count`.
- `bf_releasebuffer(view)` — decrements
  `core->exporter_count`, frees the `view->internal`
  shape/strides arrays.

The Rust integration test (`tests/capi_ndarray.rs`)
drives 14 paths, each asserting against the expected
shape:

| # | Test | Surface exercised |
|---|---|---|
| 1 | `ndarray_module_exposes_class` | `PyModule_AddObject`, `PyType_FromSpec` |
| 2 | `ndarray_class_has_dunders` | dunder shim installation |
| 3 | `ndarray_constructor_and_repr` | `tp_init` → `tp_repr` |
| 4 | `ndarray_dict_inspection` | instance `__dict__` round-trip |
| 5 | `ndarray_shape_property` | `tp_getset` |
| 6 | `ndarray_fill_and_sum` | `tp_methods` (METH_O, METH_NOARGS) |
| 7 | `ndarray_sequence_len_and_item` | `sq_length` + `sq_item` |
| 8 | `ndarray_setitem_and_subscript` | `mp_subscript` + `mp_ass_subscript` (with tuple keys) |
| 9 | `ndarray_iter_walks_rows` | `tp_iter` + `tp_iternext` (re-entrant `PyObject_CallObject`) |
| 10 | `ndarray_addition_via_dunder` | `nb_add` + `make_like` re-entry |
| 11 | `ndarray_buffer_size_function` | `bf_getbuffer` + `PyBuffer_SizeFromFormat` |
| 12 | `ndarray_to_bytes_round_trip` | `PyBytes_FromStringAndSize` |
| 13 | `ndarray_format_size_helper` | `buffer_format::FormatSpec` |
| 14 | `ndarray_skipped_when_extension_missing` | env-var-gated skip |

All 14 pass on `main` after this RFC lands.

## Implementation status (post-merge)

| Area | LOC (Rust) | Status |
|------|-----------:|--------|
| `slottable.rs` (slot table + IDs) | ~600 | ✅ |
| `buffer.rs` + `buffer_format.rs` (Py_buffer, format parser) | ~900 | ✅ |
| `memoryview.rs` (PyMemoryView_*) | ~400 | ✅ |
| `vectorcall.rs` (PEP 590) | ~250 | ✅ |
| `types.rs` (PyType_FromSpec[WithBases], heap registry) | ~1500 | ✅ |
| `dunder_shim.rs` (Python-level dunder synthesis) | ~1100 | ✅ |
| `getset.rs` + `genericalloc.rs` (descriptor + alloc helpers) | ~400 | ✅ |
| `interp.rs` (cross-crate live VM pointer) | ~150 | ✅ |
| `module.rs` (`wrap_c_method_function` + `_capi:` prefix) | ~250 | ✅ |
| `Python.h` (slot ID + struct declarations) | +~700 | ✅ |
| `force_link_table.rs` (export pinning) | +~80 | ✅ |
| `_ndarray.c` integration fixture | ~370 (C) | ✅ |
| `tests/capi_ndarray.rs` Rust integration tests | ~370 | ✅ |
| `tests/capi_buffer.rs` Rust unit tests | ~210 | ✅ |
| Workspace `cargo test` green (200+ tests) | — | ✅ |
| `cargo clippy --workspace --all-targets -D warnings` clean | — | ✅ |
| README "Status" updated to mention C-extension entry | — | ✅ |

## Drawbacks

- **Heap-type registry is a process-wide static.** Every
  `PyType_FromSpec` call grows it. Lookup is linear; the
  cost is O(n) per `Object::Instance` materialisation.
  For realistic extension counts (numpy ships ~40 heap
  types; a typical app dlopens ~5-10 extensions for
  ~100-400 heap types total) the lookup is sub-microsecond.
  We accept the cost; if it becomes a bottleneck under
  profiling we can switch to a `HashMap<*const TypeObject, *mut PyTypeObject>`.
- **Buffer protocol snapshot semantics.** Our
  `PyMemoryView_FromObject(non_buffer_aware)` copies the
  exporter's bytes into a Rust-owned `Vec<u8>` rather
  than retaining a live exporter view. CPython retains
  a live view; consumers that mutate the original
  exporter and expect the memoryview to reflect the
  change will see stale data. This matters for
  buffer-aware exporters that use the memoryview as
  a writeable handle. The `_ndarray.c` fixture
  side-steps this by managing its memory through a
  `PyLong`-encoded raw pointer, but a future RFC may
  need to re-introduce a live-view path for full
  parity.
- **Re-entrant `&mut Interpreter` is technically UB.**
  We sidestep it through raw pointers and the publish
  stack, but a strict-aliasing-aware miri run would flag
  the inner call's `&mut *p` against the outer call's
  `&mut self`. The single-threaded VM model means this
  doesn't manifest at runtime. The proper fix —
  switching the VM to interior mutability — is RFC
  0030's territory.
- **`tp_traverse` / `tp_clear` are accepted but
  best-effort.** The cycle GC integrates them via
  `gc_trace::track`, but a C extension that registers
  a traverse function relying on `Py_VISIT(field)`
  walking the cycle collector's roots may not see the
  traversal it expects. Real-world consumers (numpy)
  don't depend on this for correctness, only for GC
  promptness.
- **Vectorcall fallback always succeeds.** When a
  callable lacks `Py_TPFLAGS_HAVE_VECTORCALL`, we
  silently fall back to the args-tuple path. CPython
  emits a slow-path warning under
  `Py_TRACE_REFS`; we don't. Acceptable for the
  performance-conscious extension authors.

## Alternatives

1. **Land the buffer protocol without the dunder
   shim layer.** Tempting (smaller diff), but every
   real extension that defines `nb_add` *also*
   expects `obj1 + obj2` to dispatch through Python.
   Without the shims, the C-side slots would be live
   but unreachable from Python.
2. **Implement a "C-extension parity mode" feature
   flag.** Some implementations (PyPy's `cpyext`)
   gate strict-CPython C-API behind a flag because
   their native object layout differs. We don't:
   our `PyObject` layout matches CPython's
   `Py_LIMITED_API` shape and `_PyObject_*` private
   shape closely enough that the parity is the
   default.
3. **Skip vectorcall.** PEP 590 is recent (3.9+).
   We could ship just the legacy `tp_call` path.
   Decided against: Cython 3.x emits vectorcall by
   default and the legacy fallback path doubles
   dispatch cost.
4. **Punt the `_ndarray.c` fixture; rely on
   `_smalltest.c` for coverage.** The previous
   fixture only exercised `METH_VARARGS` calls;
   it would have left every protocol slot
   silently broken. The 14-path ndarray fixture
   is the smallest end-to-end shape that
   touches every code path.

## Prior art

- **PyPy's `cpyext`.** PyPy's CPython-compat layer
  is the prior art for "make a non-CPython runtime
  dlopen CPython native extensions." Their
  `cpyext/typeobject.py` has the same shape as our
  `PyType_FromMetaclass`; their buffer protocol
  surface is the closest reference for what
  `PyObject_GetBuffer` should do for a non-CPython
  exporter. We borrow the slot-table-then-shim
  architecture directly.
- **GraalPy's `polyglot` C-API.** GraalPy embeds
  a C-API surface that bridges CPython native
  extensions to the Truffle runtime. Their
  approach to `Py_buffer` lifetime is a snapshot-
  on-acquire model; we land on the same default
  for non-buffer-aware exporters.
- **`Py_LIMITED_API` itself.** The CPython
  documentation for the limited API enumerates
  exactly which slot IDs and helpers are public.
  This RFC's surface is a strict subset of that
  enumeration.

## Future work

- **RFC 0029 — `numpy.so` end-to-end.** Build on
  this RFC's machinery to ship a `pip install numpy`
  story that "just works" out of the box. Includes
  vendoring numpy's wheel build, fixing the long
  tail of private-API consumers numpy reaches into,
  and gating CI on `python -c "import numpy; print(numpy.zeros((3, 3)))"`.
- **RFC 0030 — Interior-mutable Interpreter.** Drop
  `&mut Interpreter` from public methods in favour
  of a `&self` + interior `RefCell` model. Eliminates
  the re-entrant aliased-`&mut` concern at its
  source.
- **RFC 0031 — Live buffer views.** Replace the
  snapshot-on-acquire memoryview path with a
  retained exporter reference, matching CPython.
  Requires sorting out the `Py_buffer.obj` lifetime
  story across the cycle GC.
- **RFC 0032 — `Cython`-aware test matrix.**
  Bundle a few `.pyx` files compiled to `.so` in
  the regression suite; gate CI on every commit.
- **RFC 0033 — `pyperformance` macro suite.**
  Once `pip install pyperformance` works, bundle
  the macro suite and start tracking per-PR perf
  deltas against CPython.
