# RFC 0047: CPython 3.13 binary-ABI compatibility (cpyext) - wave 5: the Cython-generated extension surface (pandas), faithful `inherit_slots`, and the manylinux/macOS/musllinux wheel matrix

- **Author**: WeavePy core
- **Status**: Accepted
- **Part of**: the D1 binary-ABI arc whose roadmap lives in
  [RFC 0043](0043-cpython-binary-abi.md). RFC 0043 is the umbrella/roadmap
  RFC; this is the detailed-design RFC for **wave 5**, the final wave.
- **Builds on**: RFC 0043 (wave 1 - the layout-faithful object mirror, the
  byte-faithful `PyTypeObject`, the immortal-refcount sentinel), RFC 0044
  (wave 2 - the full type-suite round trip, real `PyType_Ready`, the
  `SlotTable` -> `dunder_shim` finalisation, GC integration), RFC 0045
  (wave 3 - faithful inline `tp_basicsize` instance storage, real
  `tp_members`, the array-interchange + C-API-capsule surface), RFC 0046
  (wave 4 - real numpy from source, the foreign-object soul, the
  faithfulness hardening, and the `tp_base`-walk approximation of
  `inherit_slots`).

## Summary

Wave 4 made the single densest C-API consumer in the ecosystem - **real
numpy** - import and compute against WeavePy's faithful host ABI. Wave 5
closes the gap between "numpy imports" and "the rest of the binary-wheel
ecosystem runs", whose dominant shape is **Cython-generated** code:
pandas is ~70% Cython by line count, and Cython's runtime is what lxml,
pydantic-core's helpers, scikit-learn, pyarrow's Python layer, and
thousands of smaller wheels are built on. Cython exercises one corner of
the ABI that hand-written extensions and even numpy mostly avoid, and it
exercises it *constantly*: it reads type slots **directly off the C
struct** (`Py_TYPE(self)->tp_as_number->nb_add`, `…->tp_repr`,
`…->tp_as_sequence->sq_length`), with no MRO walk, on instances of
**subclasses it defines**.

Wave 5 is four workstreams plus a hermetic proof:

1. **Faithful `inherit_slots`** (the wave-4 deferred item, RFC 0046
   §2.7 / Known limitations). CPython finishes `PyType_Ready` by copying
   every `tp_*` function slot and method-suite entry a subtype leaves
   NULL down from its base, so an inlined `Py_TYPE(sub)->tp_repr` on a
   subclass resolves to the base's function. Waves 1-4 did not: a readied
   subtype carried only the slots it spelled out itself, and inherited
   behaviour was reachable *only* through the bridged MRO (the synthesised
   dunder shims) - correct for Python-level dispatch, a NULL-deref for the
   Cython idiom. Wave 5 bakes the inherited slots into both the decoded
   `SlotTable` and the faithful `PyTypeObject` struct at ready time.
   (`crates/weavepy-capi/src/inherit.rs`, `src/types.rs`.)

2. **The Cython C-API runtime tail.** The leaf entry points Cython's
   `Cython/Utility/*.c` runtime links that waves 1-4 had not yet exported
   - found the same way wave 4 found numpy's tail: diff a Cython
   `.cpython-313-*.so`'s undefined `Py*`/`_Py*` symbols against the host's
   dynamic table. (`crates/weavepy-capi/src/wave5.rs`.)

3. **A real vectorcall defect** Cython's method-call shims exposed. Every
   `obj.method(arg)` Cython emits goes through the stock inline
   `PyObject_CallMethodOneArg`, which calls our `PyObject_VectorcallMethod`
   with `PY_VECTORCALL_ARGUMENTS_OFFSET` set. WeavePy's vectorcall
   argument decoder mis-read that flag as a one-slot **shift** of the
   `args` array (it only marks `args[-1]` as scratch), so it read one past
   the end and dereferenced garbage. Fixed in
   `crates/weavepy-capi/src/vectorcall.rs`.

4. **The wheel matrix.** `_packaging` already enumerated the manylinux and
   macOS platform tags; wave 5 adds **musllinux** (PEP 656, the Alpine/musl
   sibling numpy and pandas both ship) and a WeavePy **provenance** tag so a
   publisher can ship a build verified against WeavePy specifically, plus
   the matching musl SOABI suffixes in the extension loader.
   (`crates/weavepy-vm/src/stdlib/python/_packaging.py`,
   `crates/weavepy-capi/src/loader.rs`.)

The proof is hermetic: `tests/capi_ext/_stockcython.c`, compiled against
the host's **stock CPython 3.13 headers** and shaped like a Cython
extension (an extension-defined base, a pure subclass, and a
partial-override subclass that reads inherited slots straight off
`Py_TYPE(self)`), `dlopen`ed into WeavePy and driven by
`capi_stockcython.rs` (7 tests).

## Motivation

The binary-ABI arc's ceiling is "the wheels users actually `pip install`
run unmodified." Wave 4 cleared numpy, but numpy is hand-written C: it
reads its *own* inline fields (wave 3) and a large flat C-API surface
(wave 4), yet it rarely defines a *subclass of an extension type* and then
reads that subclass's slots off the struct. Cython does both, by
construction, for every `cdef class`:

- A `cdef class Sub(Base)` compiles to a static `PyTypeObject` whose
  `tp_base = &Base_Type` and whose slots are mostly NULL when `Sub`
  doesn't override them.
- Cython's generated call sites do **not** go through `PyObject_GetAttr` +
  the MRO. They inline the slot read: `Py_TYPE(self)->tp_as_number->nb_add`,
  `Py_TYPE(self)->tp_iternext`, `Py_TYPE(self)->tp_descr_get`. This is the
  whole point of Cython - it bypasses Python's dispatch.

On a subclass whose `tp_as_number` was left NULL, that inlined read is a
NULL-deref before the first line of user code. RFC 0046 shipped a per-call
`tp_base` walk for the `tp_repr`/`tp_str` path as a stop-gap and named the
real fix - bake the inherited slots in at ready time, as CPython does - as
wave-5 work. This RFC is that fix, generalised to *every* slot and method
suite, plus the leaf symbols and the method-call fast path Cython needs to
link and run.

The wheel-matrix piece is the distribution dual: an ABI that hosts a
Cython wheel is only useful if the resolver recognises the wheel. numpy
and pandas publish across manylinux, macOS (`universal2`/`x86_64`/`arm64`),
**and** musllinux; the last was the one platform family `_packaging` did
not enumerate.

## The central problem, precisely

CPython's `PyType_Ready` runs `inherit_slots` (`Objects/typeobject.c`) as
its final step. For every slot the subtype leaves NULL, it copies the
base's value down - the `COPYSLOT` / `COPYNUM` / `COPYSEQ` / `COPYMAP`
macro family. The result is that a finalised subtype's struct is
**flattened**: `Sub_Type.tp_repr`, `Sub_Type.tp_as_number->nb_add`, and so
on all point at the function that will actually run, whether the subtype or
an ancestor defined it. Inlined reads off the struct therefore always land
on real code.

WeavePy's `PyType_Ready` (wave 2) instead harvests the subtype's *own*
slots into a `SlotTable`, builds a bridged native `TypeObject` whose MRO
carries the inherited behaviour as synthesised dunder shims, and writes
`ob_type` + the ready flag back into the struct. It never flattened the
struct. Two consequences:

- **Python-level dispatch is correct.** `sub + other` resolves
  `Base.__add__` through the bridged MRO exactly as CPython resolves it
  through the type's `__mro__`. Wave 2's fixtures (all of which subclass
  `object`) never noticed the gap.
- **Direct struct reads on a subclass are wrong.** `Py_TYPE(sub)->tp_repr`
  is NULL if `Sub` didn't define `__repr__`; `…->tp_as_number` is NULL if
  `Sub` declared no number suite. The Cython idiom dereferences exactly
  these.

The instance side is already faithful for this: an instance of a readied
subclass crosses into C with `ob_type` set to the subclass's own
`PyTypeObject*` (`find_type_ptr` resolves a readied type's `ext_ptr`), so
`Py_TYPE(sub_instance)` *is* `&Sub_Type`. The only missing piece is making
`&Sub_Type`'s slots non-NULL - i.e. `inherit_slots`.

## CPython reference

- **`inherit_slots`** (`Objects/typeobject.c`). Runs once per type at the
  end of `PyType_Ready`, after `mro` and `inherit_special`. Copies every
  function slot and method-suite entry from `tp_base` (more precisely, from
  the MRO, but because each base is itself readied-and-flattened first, the
  immediate base suffices) when the subtype's is NULL. The method suites
  (`tp_as_number`, `tp_as_sequence`, `tp_as_mapping`, `tp_as_async`,
  `tp_as_buffer`) are merged field-by-field via the `COPYSLOT` macros.
- **The `PY_VECTORCALL_ARGUMENTS_OFFSET` contract** (`Include/cpython/
  abstract.h`, `Objects/call.c`). The flag in `nargsf` tells the callee it
  may temporarily overwrite the scratch slot at `args[-1]` (CPython's trick
  for prepending `self` without reallocating). It does **not** shift
  `args`: `args[0]` is always the first argument, and
  `PyVectorcall_NARGS(nargsf) = nargsf & ~OFFSET` counts the elements at
  `args[0..nargs]`. `PyObject_CallMethodOneArg(self, name, arg)` calls
  `PyObject_VectorcallMethod(name, {self, arg}, 2 | OFFSET, NULL)`:
  `args[0]` is `self`, `args[1]` is the argument, `nargs == 2`.
- **`_PyObject_GetMethod`** (`Objects/object.c`). Returns 1 and an
  *unbound* function when `name` is a plain method on the type (so the
  caller passes `self` as arg 0), 0 and an already-*bound* attribute
  otherwise. Both returns are valid; the unbound case is a micro-opt.

## Detailed design (wave 5)

### Workstream 1: faithful `inherit_slots`

`crates/weavepy-capi/src/inherit.rs` adds one entry point, called from
`PyType_Ready` immediately after the bridged `TypeObject` is built (so the
type dict still carries only the subtype's *own* dunders - inherited
behaviour stays reachable through the MRO, exactly as CPython keeps it):

```rust
pub unsafe fn inherit_slots(
    t: *mut PyTypeObject,
    table: &mut SlotTable,
    base: *mut PyTypeObject,
)
```

It copies, **from the immediate base only**:

1. **The decoded `SlotTable`.** Every slot id the subtype left NULL is
   filled from the base's table, so the direct-table-read dispatch paths
   (the buffer protocol, vectorcall, `tp_descr_get`/`set`, the GC bridge)
   and the `has_*_protocol` queries see the inherited slot.
2. **The faithful `PyTypeObject` struct.** Every NULL direct function slot
   (`tp_repr`, `tp_hash`, `tp_call`, `tp_iter`/`tp_iternext`,
   `tp_richcompare`, `tp_descr_get`/`set`, `tp_init`/`tp_new`,
   `tp_traverse`/`tp_clear`, the `tp_dealloc` destructor, …), the
   instance-layout offsets (`tp_dictoffset`, `tp_weaklistoffset`,
   `tp_vectorcall_offset`) when the subtype adds no storage of its own, and
   every method suite is filled in place. A suite is **shared** (pointer
   copied) when the subtype has none, or **merged word-by-word** (every
   NULL field filled from the base) when the subtype declares its own
   partial suite - CPython's per-slot `COPYSLOT`, exploiting that every
   method-suite field is pointer-width.

Copying only from the *immediate* base is sufficient and complete:
`PyType_Ready` readies a type's base before the type itself
(`bridge_or_ready(tp_base)` during harvest), and each base was itself run
through `inherit_slots`, so the immediate base's table and struct are
already fully flattened. One level of copy therefore carries the whole
ancestor chain - the same invariant CPython relies on. When the base is a
WeavePy-native builtin (whose behaviour the VM provides through the MRO,
not through C slots) or null, this is a no-op.

This **supersedes** RFC 0046 §2.7's per-call `tp_base` walk for
`repr`/`str`: with the slots baked in, `PyObject_Repr`/`Str` on a foreign
subclass read the inherited slot directly off the (now-flattened) struct.
The §2.7 walk is left in place as a defensive fallback (it is a no-op once
the slot is non-NULL) and is a candidate for removal in cleanup.

### Workstream 2: the Cython C-API runtime tail

`crates/weavepy-capi/src/wave5.rs` adds the leaf functions Cython's
runtime links, each a thin delegator onto the wave-1/2/3 surface or a
sound no-op under WeavePy's object model:

- **`_PyObject_GetDictPtr`** -> `NULL`. WeavePy keeps an instance's
  `__dict__` in a managed Rust cell, not at a fixed `tp_dictoffset` inside
  the object body, so there is no in-body `PyObject **dictptr` to hand
  back. CPython itself returns NULL for any type with `tp_dictoffset == 0`,
  and every caller (Cython's `__Pyx_GetAttr*`, CPython's
  `_PyObject_GenericGetAttrWithDict`) treats NULL as "no fast dict" and
  falls back to `tp_getattro` / `PyObject_GenericGetAttr`, which WeavePy
  services. A faithful in-body `tp_dictoffset` dict is a documented
  Non-goal.
- **`PyObject_GetOptionalAttrString` / `PyMapping_GetOptionalItem` /
  `PyMapping_GetOptionalItemString`** (CPython 3.13 additions). The
  "present -> 1, absent -> 0 with the error cleared" probes Cython uses for
  optional attribute/item access. Each delegates to the existing
  get-attr/get-item and maps a missing value to absence.
- **`_PyObject_GetMethod` + `PyObject_CallMethodOneArg`** - the fast
  method path. `_PyObject_GetMethod` resolves through the VM binding
  protocol and returns 0 ("bound"); Cython's `__Pyx_PyObject_GetMethod`
  handles both the bound and unbound returns, so never taking the unbound
  micro-opt branch is sound. (`PyObject_CallMethodOneArg` itself is the
  stock inline shim -> `PyObject_VectorcallMethod`; see WS3.)
- **`_PyDict_NewPresized`** -> `PyDict_New` (the presize hint is
  informational - WeavePy's dict grows on demand).
- **`PyLong_AsInt`** - the 3.13 public spelling of the bounds-checked
  `int` conversion (`__Pyx_PyInt_As_int`), delegating to `PyLong_AsLong`
  with a range check.
- **`PyImport_ImportModuleLevelObject`** - the entry behind Cython's
  `__Pyx_Import`. Services the absolute-import form (`level == 0`); the
  relative form (`level > 0`) is a documented bound.

(The interned-string fast-compares `PyUnicode_EqualToUTF8[AndSize]` Cython
also links were already implemented in `strings.rs`; wave 5 does not
duplicate them.) `src/force_link_table.rs` gains a `#[used]` anchor for
each new leaf so it survives into the dynamic symbol table for `dlopen`.

### Workstream 3: the vectorcall `ARGUMENTS_OFFSET` defect

Cython compiles `obj.method(arg)` to the stock inline
`PyObject_CallMethodOneArg`, which calls
`PyObject_VectorcallMethod(name, {self, arg}, 2 | PY_VECTORCALL_ARGUMENTS_OFFSET, NULL)`.
WeavePy's three vectorcall argument decoders (`collect_positional`,
`collect_positional_after`, `kwnames_to_dict`) treated the OFFSET bit as a
**+1 index shift** of the `args` array - reading `args[offset..]` rather
than `args[0..]`. That is a misreading of the contract: the bit only marks
`args[-1]` as scratch; `args[0]` is still the first argument. With OFFSET
set, the method decoder read one element past the end of `{self, arg}` and
fed the garbage pointer to `clone_object`, faulting on a misaligned
dereference.

The fix removes the shift entirely (the corrected contract is documented
on the const and module): `collect_positional` reads `args[0..nargs]`,
`collect_positional_after` (the `VectorcallMethod` receiver-skip) reads
`args[1..nargs]`, and keyword values sit at `args[nargs..nargs+nkw]`. This
was latent because WeavePy's *own* vectorcall callers
(`call_via_vectorcall`) never set the OFFSET bit and never reserve a
leading slot - so the bug only triggered for external (stock/Cython
inline-shim) callers, which is precisely the wave-5 surface.

### Workstream 4: the wheel matrix

`crates/weavepy-vm/src/stdlib/python/_packaging.py`:

- **musllinux** (PEP 656). `_platform_tags()` now emits
  `musllinux_1_{0..5}_{machine}` on Linux alongside the manylinux range -
  numpy and pandas both publish `musllinux_1_1` and `musllinux_1_2` wheels,
  and without these the resolver skipped every binary wheel on a musl host.
  `_platform_tags()` also gained optional `plat`/`machine` parameters for
  host-independent testing.
- **Provenance.** `compatible_tags()` emits a WeavePy interpreter tag
  (`weavepy`) ahead of the stock `cp313`/`abi3` tags, and `wheel_score()`
  ranks a `weavepy`-tagged wheel above the generic stock build it shadows.
  This lets a project ship a build verified against WeavePy specifically
  (e.g. `pkg-1.0-weavepy-cp313-<plat>.whl`) that stock CPython never sees
  (it emits no `weavepy` tag) but WeavePy prefers. The provenance tags are
  gated on `sys.implementation.name == 'weavepy'`, so the module stays
  byte-for-byte CPython-faithful if vendored elsewhere.

`crates/weavepy-capi/src/loader.rs`: `extension_suffixes()` recognises the
musl SOABI suffixes (`.cpython-313-{x86_64,aarch64}-linux-musl.so`)
alongside the existing glibc ones, so a wheel from either Linux ABI
resolves to its `.so`.

## The hermetic proof: `_stockcython`

`tests/capi_ext/_stockcython.c` is compiled against the host's stock
CPython 3.13 headers (`build.rs`, skipped with a warning when the dev
headers aren't present, so a bare CI host still builds) and is shaped like
a Cython extension:

- **`CyBase`** - a base type defining a number suite (`nb_add`), a sequence
  suite (`sq_length`), `tp_repr`, `tp_hash`, and `tp_richcompare`.
- **`CySub(CyBase)`** - a *pure* subclass that declares **nothing**. Every
  slot it dispatches must come from `CyBase` via `inherit_slots`.
- **`CySub2(CyBase)`** - a *partial-override* subclass with its own
  `tp_repr` and a number suite carrying only `nb_subtract`. `inherit_slots`
  must keep its own `tp_repr`/`nb_subtract` and **merge** `nb_add` into that
  same suite from the base (the in-place suite-merge path).

`probe_slots(obj)` reads the slots **directly off `Py_TYPE(obj)`** - the
inlined Cython idiom, no MRO - and invokes them, returning a result dict.
`cython_runtime_surface(obj)` exercises the WS2 tail (the optional-attr/item
probes, `_PyObject_GetMethod`, the method-one-arg call that flows through
the WS3-fixed vectorcall path, the presized dict, `PyLong_AsInt`, and the
NULL `_PyObject_GetDictPtr`). `capi_stockcython.rs` drives both and asserts
the inherited/own slot split per subclass, that the directly-read slots
compute correctly, and that the Python-level MRO dispatch on a subclass is
undisturbed.

## Measured targets

The commit-acceptance bar for wave 5:

- A stock-CPython-3.13-headers extension (`_stockcython`) that **subclasses
  an extension-defined base** and reads the inherited slots directly off
  `Py_TYPE(self)` loads via `dlopen` and runs: a pure subclass resolves
  every inherited slot off its own struct, and a partial-override subclass
  keeps its own slots while the base's are merged in.
- The Cython C-API runtime tail links and behaves (optional probes, the
  method fast path, presized dict, bounds-checked int, the NULL dict-ptr
  fallback).
- A Cython-style method call (`obj.method(arg)` through the stock inline
  `PyObject_CallMethodOneArg`) dispatches correctly - the vectorcall
  `ARGUMENTS_OFFSET` defect is fixed.
- The wheel resolver recognises the full manylinux / macOS / **musllinux**
  matrix, plus the WeavePy provenance tag, and the loader recognises the
  musl SOABI suffixes.
- The wave-1/2/3/4 fixtures and the whole `weavepy-capi` suite stay green
  through the `inherit_slots` and vectorcall changes.
- `cargo build --workspace`, `cargo fmt --check`, and
  `cargo clippy --workspace --all-targets -- -D warnings` are green; the
  regrtest sweep stays behaviourally `--check` clean on the release binary.

## Measured outcome

Landed as designed. The hermetic proof fixture `_stockcython` (stock
CPython 3.13 headers, no WeavePy header) passes **7/7** in
`capi_stockcython.rs`:

- `stockcython_base_slots_direct` - baseline: `CyBase`'s own slots are
  directly readable off `Py_TYPE(self)` and compute correctly (the probe is
  sound).
- `stockcython_pure_subclass_inherits_all_slots` - the headline: `CySub`,
  which declares nothing, has `tp_repr`/`tp_hash`/`tp_richcompare` and the
  `nb_add`/`sq_length` suite entries all non-NULL on its **own** struct
  (each was NULL pre-wave-5), and invoking them directly yields the base's
  results (`repr "CyBase(7)"`, `hash 7`, `len 7`, `nb_add(7,7) == 14`).
- `stockcython_partial_subclass_merges_suite` - `CySub2` keeps its own
  `tp_repr` (`"CySub2(9)"`) and `nb_subtract` (`9-9 == 0`) while `nb_add`
  is merged **into its own number suite** from the base (`9+9 == 18`) and
  `tp_hash`/`sq_length` are inherited - the in-place `COPYSLOT` merge.
- `stockcython_python_level_dispatch_on_subclass` - `inherit_slots` does
  not disturb the MRO path: `len(sub)`, `sub + sub`, `repr(sub)`,
  `sub == sub` all still resolve through the bridged dunders.
- `stockcython_runtime_surface` - the WS2 tail + the WS3-fixed method call:
  `dictptr_null`, optional-present/absent, `_PyObject_GetMethod`,
  `PyObject_CallMethodOneArg` (`sub.__eq__(sub)` truthy),
  `_PyDict_NewPresized` + `PyMapping_GetOptionalItemString`, and
  `PyLong_AsInt(4242)` all return the expected values.
- `stockcython_module_loads_with_types`,
  `stockcython_skipped_when_extension_missing` - module/skip plumbing.

No regressions: the entire `weavepy-capi` suite is green - **106 tests, 0
failed** across the lib unit tests (23) and every fixture binary
(`capi_buffer` 10, `capi_loader` 6, `capi_ndarray` 14, `capi_numpylike` 14,
`capi_stockabi` 9, `capi_stockcython` 7, `capi_stocktype` 11,
`capi_stockarray` 11, `capi_wheel_endtoend` 1). The wave-5 `inherit_slots`
flattening and the vectorcall decoder fix left the wave-1/2/3/4 fixtures
untouched; the vectorcall fix in particular only changed behaviour for
external callers that set `ARGUMENTS_OFFSET`, which WeavePy's own paths
never do.

The wheel matrix is covered by `tests/regrtest/test_packaging_pep440.py`
(`test_wheel_matrix_wave5` asserts manylinux + musllinux + macOS tags;
`test_wheel_provenance_wave5` asserts the `weavepy` provenance tag is
emitted, accepted, and out-scores the stock wheel it shadows) - **8/8**
green under WeavePy.

Hygiene: `cargo build --workspace --all-targets`, `cargo fmt --all
--check`, and `cargo clippy --workspace --all-targets -- -D warnings` are
clean. The curated regrtest conformance sweep shows zero behavioural
regressions (wave 5's C-API changes are reached only through the binary-ABI
path; the packaging additions are exercised by the PEP 440/425 regrtest).

## Non-goals / deferred

- **Building pandas from source in CI** is not the gate. pandas's build
  graph (a full Cython + C + C++ toolchain, plus numpy as a build-time
  dependency) is heavier than the wave-4 numpy gate and adds little ABI
  coverage beyond the hermetic `_stockcython` proof and the live numpy
  gate. Wave 5's acceptance is the hermetic fixture + the retained numpy
  gate; a pandas-from-source CI lane is good follow-up infrastructure, not
  an ABI question.
- **In-body `tp_dictoffset` `__dict__`.** WeavePy stores an instance's
  `__dict__` in a managed Rust cell, so `_PyObject_GetDictPtr` returns NULL
  and callers take the generic-getattr fallback (which is correct). A
  faithful in-body dict at a declared `tp_dictoffset` is not implemented.
- **Relative imports through `PyImport_ImportModuleLevelObject`**
  (`level > 0`). The absolute form is serviced; the relative form (Cython's
  `from . cimport`) is bounded, as a hermetic single-module extension does
  not use it.
- **The unbound `_PyObject_GetMethod` micro-optimisation.** WeavePy always
  returns the bound form (return 0); correct, just never the fast unbound
  branch.
- **Multi-threaded / free-threaded (`Py_GIL_DISABLED`)** Cython extensions.
- **A fully general foreign-metaclass `__getattribute__`/`tp_call` path.**
  Wave 4 implemented the getset-on-metatype case (RFC 0046 §2.10); the
  general metaclass-call path remains future work.

## Alternatives

1. **Keep the per-call `tp_base` walk (RFC 0046 §2.7) and generalise it to
   every slot.** Rejected: it pays an MRO-length walk on every slot read
   (Cython reads slots in hot loops), it has to be duplicated at every
   dispatch site, and it does not help a consumer that reads the struct
   field *directly* (the Cython idiom) rather than going through a WeavePy
   dispatch function. Baking the slots in at ready time is what CPython
   does and is both faster and complete.
2. **Honour `PY_VECTORCALL_ARGUMENTS_OFFSET` as a real array shift** (the
   status-quo decoder behaviour). Rejected: it is a misreading of the
   contract - the bit concerns the scratch slot `args[-1]`, never the index
   of `args[0]` - and it reads out of bounds for every external method-call
   shim. The fix is to stop shifting.
3. **Implement an in-body `tp_dictoffset` dict to satisfy
   `_PyObject_GetDictPtr` literally.** Rejected for wave 5: returning NULL
   is itself a faithful CPython answer (for `tp_dictoffset == 0`) and every
   caller has a correct fallback, so the in-body dict buys no Cython
   compatibility for the cost of a second instance-layout model.

## Prior art

- **PyPy `cpyext`.** Faces the same direct-slot-read problem and solves it
  by materialising a flattened `PyTypeObject` for any type that crosses
  into C, copying the slots from the app-level type. Wave 5's `inherit_slots`
  is the static-type analogue: flatten the readied struct once at ready
  time.
- **CPython `Objects/typeobject.c::inherit_slots`** is the reference; wave 5
  reproduces its immediate-base `COPYSLOT` flattening over WeavePy's split
  representation (decoded `SlotTable` + faithful struct).
- **Cython's `Cython/Utility/ObjectHandling.c`** is the consumer whose
  inlined `Py_TYPE(o)->tp_*` reads motivate the whole workstream.

## Future work

- Remove the now-redundant §2.7 `tp_base` walk once a release has shipped
  with baked-in `inherit_slots`.
- A pandas-from-source CI lane (analogous to the wave-4 numpy gate).
- The relative-import (`level > 0`) form of
  `PyImport_ImportModuleLevelObject`.
- Free-threaded Cython extensions, paired with the broader
  `Py_GIL_DISABLED` work.

## Files

New:

- `crates/weavepy-capi/src/inherit.rs` - faithful `inherit_slots`
  (decoded-table + faithful-struct flattening from the immediate base).
- `crates/weavepy-capi/src/wave5.rs` - the Cython C-API leaf tail.
- `tests/capi_ext/_stockcython.c` - the hermetic Cython-shaped proof
  (stock CPython 3.13 headers).
- `crates/weavepy-capi/tests/capi_stockcython.rs` - the integration test
  (7 tests).

Materially changed:

- `crates/weavepy-capi/src/types.rs` - call `inherit_slots` from
  `PyType_Ready` after the bridged type is built.
- `crates/weavepy-capi/src/vectorcall.rs` - the `ARGUMENTS_OFFSET`
  decoder fix (no array shift) + corrected docs.
- `crates/weavepy-capi/src/loader.rs` - musl SOABI extension suffixes.
- `crates/weavepy-capi/src/force_link_table.rs` - `#[used]` anchors for the
  wave-5 tail; `crates/weavepy-capi/src/lib.rs` - module wiring;
  `crates/weavepy-capi/build.rs` - compile `_stockcython` against the stock
  headers.
- `crates/weavepy-vm/src/stdlib/python/_packaging.py` - musllinux tags,
  the WeavePy provenance tag, host-parameterised `_platform_tags`.
- `tests/regrtest/test_packaging_pep440.py` - wave-5 wheel-matrix +
  provenance tests.
