# RFC 0046: CPython 3.13 binary-ABI compatibility (cpyext) - wave 4: real numpy from source against the faithful host ABI

- **Author**: WeavePy core
- **Status**: Accepted
- **Part of**: the D1 binary-ABI arc whose roadmap lives in
  [RFC 0043](0043-cpython-binary-abi.md). RFC 0043 is the umbrella/roadmap
  RFC; this is the detailed-design RFC for **wave 4**.
- **Builds on**: RFC 0043 (wave 1 - the layout-faithful object mirror, the
  byte-faithful `PyTypeObject`, the immortal-refcount sentinel), RFC 0044
  (wave 2 - the full type-suite round trip, real `PyType_Ready`, the
  `SlotTable` -> `dunder_shim` finalisation path, GC integration), RFC 0045
  (wave 3 - faithful inline `tp_basicsize` instance storage, real
  `tp_members`, the array-interchange + C-API-capsule surface, the
  `_stockarray.c` fixture).

## Summary

Waves 1-3 built the faithful binary ABI *and proved it against a hermetic,
hand-written fixture* (`_stockarray.c`) that is shaped like numpy but is not
numpy. Wave 4 removes the fixture and points the same ABI at the real thing:
**a from-source build of numpy 2.5.0's `_multiarray_umath` /
`_umath_linalg` C extensions, loaded with the pure-Python numpy shim
disabled (`WEAVEPY_NO_NUMPY_SHIM=1`)**, and gates CI on the RFC 0043
acceptance one-liner:

```python
import numpy
numpy.zeros((3, 3)) @ numpy.ones((3, 3))
```

`import numpy` runs the stock package unmodified - including its `__init__`
self-checks (`_core._multiarray_umath._sanity_check`, `_mac_os_check`'s
`polyfit`/`lstsq` round-trip, and the BLAS FP-exception probe). The matmul
flows through numpy's real ufunc dispatch into the linked BLAS `dgemm`, and
the result is a real `ndarray` with the correct contents (verified beyond the
all-zeros gate: `arange(9).reshape(3,3) @ (eye(3)*2)` returns the expected
`2x` scaling, `eye(3) @ ones((3,3))` sums to 9.0).

Getting there was two pieces of work:

1. **The symbol tail.** The leaf C-API entry points `_multiarray_umath`
   links that waves 1-3 had not yet exported - discovered by diffing the
   extension's undefined `Py*`/`_Py*` symbols against the host binary's
   dynamic symbol table. Most delegate to the existing abstract/number/
   container surface; a handful are sound no-ops under WeavePy's
   single-threaded-GIL, non-tracemalloc runtime; the variadic members
   (`PyOS_snprintf`, `PyErr_WarnFormat`, ...) live in C (`src/varargs.c`)
   because they cannot be expressed as Rust `extern "C"` definitions.
   (`crates/weavepy-capi/src/wave4.rs`, `src/varargs.c`,
   `src/force_link_table.rs`.)

2. **The faithfulness hardening.** Real numpy exercises corners of the ABI
   that the fixture never did - builtin-subclass scalar construction,
   singleton pointer identity (`np._NoValue`), the C truthiness protocol on
   foreign scalars, the object-lifecycle interaction between numpy's inlined
   `Py_DECREF` and WeavePy's instance bodies, subscript dispatch onto foreign
   iterators, and `repr`/`str` slot dispatch on foreign objects. Each was a
   real soundness or correctness gap in the wave 1-3 bridge; wave 4 closes
   them. These are the substance of this RFC.

The acceptance proof is the stock package itself: no numpy source is patched,
no self-check is gated, and the process exits 0.

## Motivation

The wave-3 fixture (`_stockarray.c`) was written *to the ABI we had built*.
It is honest about layout and capsules, but a fixture cannot surprise its
author: it never constructs a `float`-subclass scalar, never compares a
sentinel by identity across the boundary, never asks "is this foreign scalar
truthy?" through the number protocol, and never frees an array from inside an
inlined `Py_DECREF` that numpy emitted from a macro. Real numpy does all of
these in the first 200 milliseconds of `import numpy`, before the user types
a single expression.

So wave 4's value is not "more symbols" (though it is that too). It is that
running the real extension is the only test that exercises the ABI the way a
wheel does, and it surfaced a cluster of latent defects - several of them
memory-safety bugs (a double-free, a use-after-free, a leaked pending
exception) that the fixture's narrower usage had hidden. Fixing them is what
makes the host ABI *actually* faithful rather than fixture-faithful.

This is the last wave before third-party wheels at large (pandas, Cython
extensions, the wheel matrix - wave 5): numpy is the densest single consumer
of the CPython C-API in the ecosystem, so an ABI that hosts it unmodified is
the credible foundation for the rest.

## Workstream 1: the symbol tail

### How the tail was found

A from-source `_multiarray_umath.cpython-313-darwin.so` has an undefined-symbol
list (`nm -u`) of every `Py*`/`_Py*` it expects the host to provide. Diffing
that against the symbols WeavePy already exports (waves 1-3) yields the exact
*leaf* tail wave 4 must add - nothing more. This keeps the surface honest:
every function in `wave4.rs` is there because a real wheel references it, not
because the C-API has it.

### Three kinds of leaf

1. **Delegators.** The majority forward to the surface waves 1-3 already
   implement. `PySys_GetObject`, `PyImport_GetModuleDict`,
   `PyObject_GetAttrString`-family helpers, the `PyNumber_*`/`PySequence_*`
   spellings numpy uses internally, the `PyUnicode_*` accessors the dtype
   machinery calls - all reduce to existing entry points.

2. **Sound no-ops.** A handful name subsystems that have no behavioural
   meaning under WeavePy's runtime model and that CPython itself short-circuits
   when the subsystem is disabled: `tracemalloc` domain hooks
   (`_PyTraceMalloc_*`), the per-thread GIL-state dance
   (`PyGILState_Ensure`/`Release` collapse to the single interpreter thread),
   and the free-threading build's mutex shims. Each returns the value CPython
   returns with the feature off, so numpy's `#ifdef`-guarded fast paths are
   taken correctly.

3. **Borrowed-reference pins.** `PySys_GetObject` and `PyEval_GetBuiltins`
   return *borrowed* references in CPython. WeavePy mints a fresh `PyObject`
   box each time a VM value crosses the boundary, so there is no persistent
   owner to borrow from - decref'ing the freshly-minted box (as "borrowed"
   would imply) would free it and hand the caller a dangle. `wave4.rs` mints
   each such object once and **pins it for the process lifetime** (a bounded,
   per-key leak), returning the same stable pointer on every call. This both
   satisfies the borrowed contract (the caller must not decref) and gives the
   object the stable identity numpy depends on.

### The variadic members live in C

`PyOS_snprintf`, `PyErr_WarnFormat`, `PyErr_Format`'s `printf`-family and the
other C-variadic entry points cannot be written as Rust `extern "C"`
definitions (Rust has no stable variadic-definition ABI). They are defined in
`crates/weavepy-capi/src/varargs.c`, compiled by the crate's build script,
and forward into the Rust core after formatting with the platform `vsnprintf`.

### Forcing the tail to link

Because most tail functions are referenced only by the *dynamically* loaded
extension and never by WeavePy's own Rust code, the linker would garbage-collect
them from the host binary. `src/force_link_table.rs` builds a `#[used]` table of
their addresses so they survive into the dynamic symbol table and are resolvable
by `dlopen`.

## Workstream 2: faithfulness hardening (the substance)

Each subsection is a defect real numpy exposed, the root cause in the wave 1-3
bridge, and the fix. They are ordered the way `import numpy` hits them.

### 2.1 Builtin-subclass scalar construction (`np.float64(...)` -> SIGSEGV)

numpy's scalar types (`np.float64`, `np.int32`, ...) are **subclasses of the
host builtins** (`float`, `int`). Constructing one runs the builtin base's
`tp_new` (`float`'s `tp_new` is at the faithful `PyTypeObject` offset
`0x138`). Waves 1-3 left the builtin types' `tp_new` slot NULL - WeavePy
constructs its own `float`/`int`/`str` through the VM, never through a C slot
- so numpy's `float64.__new__` jumping through `PyFloat_Type->tp_new` jumped
through NULL and segfaulted.

**Fix.** `crates/weavepy-capi/src/builtin_new.rs` implements a faithful
`tp_new` for the numeric builtins: it parses the single positional argument
through the same coercion the VM `float()`/`int()` constructors use and
returns a faithful boxed scalar. The slot is installed on the builtin
`PyTypeObject`s during interpreter bring-up so a C subclass that delegates to
`PyFloat_Type->tp_new(subtype, args, kwds)` lands in real code.

### 2.2 Singleton pointer identity (`np._NoValue` -> "a float is required")

numpy uses a module-level sentinel, `np._NoValue`, as the default for
reduction arguments (`a.sum(initial=np._NoValue)`), and dispatches on it **by
pointer identity** (`if initial is _NoValue`). The sentinel is an instance of
a plain Python class, so crossing it into C produced an `Object::Instance`.
Wave 1-3's `into_owned` minted a *fresh* `PyObjectBox` on every crossing, so
`_NoValue` had a different address each time it entered C; numpy's
`initial == _NoValue` (a `PyObject*` compare) was never true, so the sentinel
was treated as a literal initial value and fed to `PyFloat_AsDouble` -> "a
float is required". This surfaced during `_mac_os_check`'s `polyfit`.

**Fix.** `crates/weavepy-capi/src/object.rs` caches the boxed identity of a
non-inline `Object::Instance` in the instance's `c_body` cell. The first
crossing mints the box (`mint_instance_box`), stores
`Object::Instance(inst.clone())` in its payload, registers it, and records the
pointer in `inst.c_body`; subsequent crossings return that same pointer with
an incremented refcount (`cached_instance_box`). `free_box` clears the cell
when the identity box is freed. The sentinel now has one stable address for
its lifetime, so numpy's identity check works.

### 2.3 The C truthiness protocol on foreign scalars (`np.bool_` -> spurious `RankWarning`)

After 2.2, `polyfit(cov=True)` raised a `RankWarning` it should not. The
culprit: `polyfit` evaluates `if rank != order and not full:`. The
sub-expression `rank != order` is an `np.bool_` scalar (a *foreign* object),
and WeavePy's truthiness for `Object::Foreign` unconditionally returned
`true`, so the warning branch fired.

**Fix.** `PyObject_IsTrue` in `crates/weavepy-capi/src/abstract_.rs` now
dispatches a foreign operand through CPython's truth protocol in order:
`nb_bool` (from `tp_as_number`), then `mp_length` (from `tp_as_mapping`), then
`sq_length` (from `tp_as_sequence`), defaulting to true only when none is
defined - exactly `PyObject_IsTrue`'s own slot order. `np.bool_(False)` now
reports false through its `nb_bool`.

### 2.4 Object lifecycle: numpy's inlined `Py_DECREF` vs. faithful bodies

`import numpy`'s `_sanity_check` (`numpy.zeros` through the ufunc path)
crashed in `convert_ufunc_arguments` reading `op->descr == NULL`. Root cause:
numpy frees arrays through the **inlined** `Py_DECREF` macro, which - when the
refcount hits zero - calls `_Py_Dealloc` directly. For a WeavePy faithful
instance body that bypassed WeavePy's lifecycle control and ran the extension
`tp_dealloc`, tearing down the array payload (`data`/`dims`/`descr`) while
WeavePy still believed it owned the object; the next access read a freed
`descr`.

**Fix.** `_Py_Dealloc` in `object.rs` routes an object that is a WeavePy
*instance body* to `free_box` instead of letting `tp_dealloc` run, so the
faithful body is reclaimed under WeavePy's ownership rules (the body's block
is owned by the native instance, not by the extension).

### 2.5 `free_box` ordering: a foreign double-free (SIGBUS)

A non-trivial matmul (`numpy.eye(3)`) crashed with SIGBUS. `free_box` decided
*how* to free a pointer by consulting the type-keyed `is_mirror` check **before**
checking whether WeavePy even owns the allocation. A foreign numpy type that
WeavePy had `PyType_Ready`'d answered `is_mirror == true`, so `free_box` called
`free_mirror` on memory numpy had malloc'd - a double-free.

**Fix.** `free_box` now checks `!is_weavepy_owned(p)` **first** (right after
invalidating the borrowed-pointer cache) and bails to the foreign path before
any type-keyed mirror/instance-body logic runs. `_Py_Dealloc` is likewise
hardened to gate its `is_instance_body` probe behind `is_weavepy_owned`, so it
never dereferences foreign-owned memory to decide a lifetime.

### 2.6 Subscript dispatch onto foreign objects (`m.flat[i::M+1] = 1`)

`numpy.eye(3)` assigns through a flat iterator: `m[:M-k].flat[i::M+1] = 1`.
`m.flat` is a foreign `numpy.flatiter`. The VM's `STORE_SUBSCR` /
`BINARY_SUBSCR` opcode handlers had no arm for `Object::Foreign`, so they fell
straight to the generic "object does not support item assignment" `TypeError`.
(A direct `f.__setitem__(...)` worked because it resolved the method through
`load_attr` and bypassed the opcode.)

**Fix.** Both opcode handlers in `crates/weavepy-vm/src/lib.rs` now special-case
a foreign target: they `load_attr` `__setitem__` (writes) / `__getitem__`
(reads) and call the bound method, falling back to the generic path only when
the method is genuinely absent (so the canonical `TypeError` is still produced
for non-subscriptable foreign objects).

### 2.7 `repr`/`str` of foreign objects, and a leaked pending exception

A foreign object's `repr`/`str` came back as the debug placeholder
`<foreign T at 0x...>` because `PyObject_Repr`/`PyObject_Str` only knew the VM
`repr_for`, which sees an opaque `Object::Foreign`. So `repr(np.float64(2.5))`
and `repr(array)` were wrong.

**Fix.** `PyObject_Repr`/`PyObject_Str` detect a foreign operand and dispatch
through its `tp_repr` / `tp_str` slot (with `str` falling back to `tp_repr`,
as CPython does). Because WeavePy's `PyType_Ready` does not run CPython's
`inherit_slots` step, a stock subclass can carry a NULL `tp_repr`; the dispatch
therefore **walks the `tp_base` chain** to recover the inherited slot,
reproducing the *effect* of `inherit_slots` for this path. With this,
`np.float64(2.5)` -> `np.float64(2.5)`, `np.int32(7)` -> `np.int32(7)`, and an
array -> `array([[...]])`.

A subtle memory-safety corollary: if the dispatched slot *raises* (returns
NULL with a pending exception) and we fall back to the placeholder, the pending
exception must be consumed - otherwise it leaks into the next VM operation and
surfaces as a spurious error far from its origin. The fallback path now takes
the pending exception before returning the placeholder.

### 2.8 Thread-local teardown at process exit (exit code 133)

Once the scalar `tp_new` worked, the process aborted at exit (rc 133): the
instance-pinning thread-local map (`STRONG`) was being touched during
thread-local destruction, after it had itself been dropped. `instance.rs`'s
`free_instance_body_hook` / `release_c_ownership` now use `STRONG.try_with(..)`
and treat a destroyed map as "nothing to unpin", so teardown is panic-free.

### 2.9 GC collection of a cycle held through C-managed memory

The wave-4 non-inline-instance identity cache (Section 2.2) lets a single
instance be reached both by its GC-tracked allocation box (from
`PyType_GenericAlloc` / `_PyObject_GC_New`) and by the cached identity box
stashed in `c_body`. A stock cycle-collecting GC type breaks a reference cycle
inside `tp_clear` with the **inlined** `Py_CLEAR(child)`, whose stock
`Py_DECREF` -> `_Py_Dealloc` -> `tp_dealloc` cascade is what runs each node's
destructor (decrementing a live-node counter, freeing its C core) exactly once.

The collector's `tp_traverse` / `tp_clear` bridge (`gc_bridge.rs`) materialised
`self` into C through the identity cache, i.e. it handed `tp_clear` *the very
box a cycle edge pointed at*, with the usual `+1`. That extra reference stopped
the `Py_CLEAR` cascade from driving the cached child box's refcount to zero
through `_Py_Dealloc`; the node was instead reclaimed later through WeavePy's
`free_box` - which is `tp_free`, **not** `tp_dealloc` - so the extension's
destructor never ran and one node leaked (`stocktype_gc_cycle_through_c_memory`
regressed to `live=1`).

**Fix.** The GC bridge borrows `self` through a new
`into_owned_with_type_uncached`, which mints a *fresh* box for a non-inline
instance and never consults or populates `c_body`. The cached cycle-child boxes
keep exactly the refcount the extension expects, so the stock `Py_CLEAR`
cascade runs each node's `tp_dealloc` once and the cycle is fully reclaimed.

The tempting alternative - routing *every* WeavePy-side `Py_DecRef`-to-zero
through `_Py_Dealloc` (CPython's `Py_DECREF` -> `_Py_Dealloc` -> `tp_dealloc`
contract) - was tried and rejected: WeavePy mints many transient per-crossing
boxes for one logical instance (method receiver, each argument, traverse/clear
borrows), and running the extension `tp_dealloc` as each transient box hits
zero over-counts the destructor catastrophically (the live counter went
*negative*). The asymmetry **"the extension's own inlined `Py_DECREF` runs
`tp_dealloc`; WeavePy's internal `free_box` does not"** is therefore
load-bearing, and the fix preserves it by keeping the bridge's borrow off the
shared identity box.

### 2.10 Foreign-metaclass attribute resolution (`repr(dtype)` / `dtype.name`)

numpy's dtype `repr`/`str`/`.name`/`.kind` all read `type(dtype)._legacy` - a
getset **property on numpy's DType metaclass** `_DTypeMeta`
(`PyArrayDTypeMeta_Type`), not on the dtype class or its MRO. WeavePy readies
each DType class (`Float64DType`) into an `Object::Type`, but recorded its
metaclass as the plain `type`, so the VM's `load_attr_type` metatype lookup
never saw `_legacy`; the access raised `AttributeError`, numpy's
`arraydescr_repr` returned NULL, and dtype display fell back to the foreign
placeholder (`<foreign Float64DType at 0x...>`).

**Fix (two parts).**

1. `PyType_Ready` (`types.rs`) now reflects a *foreign* metatype onto the
   bridged type. After readying a stock type, when `Py_TYPE(t)` is a foreign
   extension metatype (not WeavePy's `PyType_Type`, and not `t` itself), WeavePy
   readies that metatype on demand - harvesting its getsets, including
   `_legacy` / `_abstract` / the `type` property - and `set_metaclass`es it onto
   the bridged type. `load_attr_type` then resolves `type(dtype)._legacy`
   through the metatype's harvested getset, invoking the C getter with the
   DType's `ext_ptr` as `self` (`into_owned` already round-trips an
   `Object::Type` back to its registered `PyTypeObject*`, so the getter reads
   numpy's genuine `PyArray_DTypeMeta` struct). This also makes `type(dtype)`
   correctly report `numpy.dtypes.Float64DType`'s real metaclass.

2. A companion VM fix: `str()` of a foreign object had gone through `repr`
   (`Object::to_str` only knows `repr`), collapsing `str(dtype)` to the repr
   form. `Interpreter::stringify` now routes a foreign operand through the
   `tp_str` hook (`foreign::str_`), so `str` and `repr` stay distinct.

With both, `repr(dtype) == "dtype('float64')"`, `str(dtype) == "float64"`, and
`dtype.name` / `dtype.kind` are byte-correct - while `str`/`repr` of an
`ndarray` (which has its own `tp_str`/`tp_repr`) remain correct.

## The CI gate

The acceptance target builds **numpy 2.5.0** from the published sdist against
the stock CPython 3.13 headers, installs it into a venv's `site-packages`, and
runs - with WeavePy's bundled pure-Python numpy shim disabled so the real C
extension is the one imported:

```bash
PYTHONPATH="$SITE_PACKAGES" WEAVEPY_NO_NUMPY_SHIM=1 \
    weavepy -c 'import numpy; numpy.zeros((3,3)) @ numpy.ones((3,3))'
```

The gate requires:

- `import numpy` completes with **no source patches** and **no self-check
  gating** - `_sanity_check`, `_mac_os_check` (which runs `polyfit`/`lstsq`),
  and the BLAS FP-exception probe all run and pass;
- the matmul returns an `ndarray` (not the shim, not a proxy);
- the process exits 0.

Correctness is checked beyond the all-zeros one-liner (which a broken matmul
could pass by coincidence): `arange(9, dtype=float).reshape(3,3) @ (eye(3)*2)`
returns the `2x`-scaled matrix (checksum 72.0) and `eye(3) @ ones((3,3))` sums
to 9.0, confirming the product flows through numpy's ufunc dispatch into the
linked BLAS `dgemm` and back.

## Testing / acceptance

- **The gate itself** is the headline acceptance: stock numpy 2.5.0,
  self-checks live, exits clean.
- **Non-trivial numerics**: `eye`, `arange`/`reshape`, scalar reductions
  (`sum`), `polyfit`/`lstsq` (via the import-time self-check) all execute.
- **Scalar/array display**: `repr`/`str` of arrays and of `np.float64` /
  `np.int32` scalars are byte-correct (regression-guarded against the foreign
  placeholder), and `repr(dtype)` (`dtype('float64')`), `str(dtype)`
  (`float64`), `dtype.name`, `dtype.kind` all resolve through the foreign
  metaclass (Section 2.10).
- **GC of C-held cycles**: a reference cycle routed through extension-managed
  memory and broken by the type's `tp_clear` is fully reclaimed - each node's
  `tp_dealloc` runs exactly once (`stocktype_gc_cycle_through_c_memory`,
  Section 2.9).
- **No regressions** in the existing capi suite or the CPython conformance
  subset (see Verification).
- **Diagnostics removed**: the temporary `WEAVEPY_DEBUG_SEGV` fault handler,
  `WEAVEPY_DEBUG_ARR`/`NCALL`/`FOREIGN`/`REPR` trace gates, and the
  `libc` dev-dependency they needed are all gone; the build is warning-clean.

## Known limitations / deferred

These are cosmetic or non-load-bearing and explicitly **not** on the wave-4
gate. They are good first issues for wave 5 polish.

- **`inherit_slots`** is approximated, not implemented. Section 2.7 walks the
  `tp_base` chain for `tp_repr`/`tp_str` on demand; a full wave-5 fix would
  bake every inherited slot into each subtype at `PyType_Ready` time as
  CPython does, removing the need for per-call base walks.
- **`add_newdoc` warnings.** numpy emits `UserWarning: add_newdoc was used on
  a pure-python object <class 'numpy.flatiter'>` (and similar) at import,
  because WeavePy's bridged C types present a writable `__doc__`. Harmless -
  numpy continues - but a faithful read-only `__doc__` on bridged static types
  would silence them.

## Non-goals (deferred to wave 5)

- pandas and the broad Cython-generated extension surface (the full-API,
  heavily-macro'd Cython idiom).
- The manylinux / macOS wheel-matrix matrix and binary-wheel provenance.
- Faithful baked-in `inherit_slots` (Section 2.7 approximates it with a
  per-call `tp_base` walk; see Known limitations). Foreign-metaclass attribute
  resolution is now implemented for the getset-on-metatype case (Section 2.10);
  a fully general metaclass `__getattribute__`/`tp_call` path is wave 5.
- Multi-threaded / free-threaded (`Py_GIL_DISABLED`) numpy.

## Files

New:

- `crates/weavepy-capi/src/wave4.rs` - the discovered C-API leaf tail.
- `crates/weavepy-capi/src/varargs.c` - the C-variadic members.
- `crates/weavepy-capi/src/force_link_table.rs` - `#[used]` link anchors.
- `crates/weavepy-capi/src/builtin_new.rs` - faithful builtin `tp_new`.
- `crates/weavepy-capi/src/foreign.rs` / `crates/weavepy-vm/src/foreign.rs` -
  the foreign-object soul + hook bridge (repr/str/hash/truth/call/getattr/
  setattr/getitem/setitem/iter/binop/compare/get_type).

Materially changed: `object.rs` (identity caching, `_Py_Dealloc` routing,
`free_box` ordering, `into_owned_with_type_uncached` for the GC bridge),
`abstract_.rs` (foreign truthiness + repr/str dispatch), `types.rs`
(`PyType_Ready` foreign-metaclass linkage; `HEAP_TYPES` made process-global),
`gc_bridge.rs` (uncached `traverse`/`clear` borrows), `instance.rs` (TLS-safe
teardown), `weavepy-vm/src/lib.rs` (foreign subscript opcodes; `stringify`
foreign `tp_str` dispatch), `module.rs`/`numbers.rs`/`containers.rs`/`mirror.rs`
(tail support).
