# RFC 0022: C-API foundation — `Python.h`, dlopen, and a real C extension

- **Status**: Accepted
- **Authors**: WeavePy authors
- **Created**: 2026-05-24
- **Tracking issue**: TBD

## Summary

Add the missing piece between "WeavePy can run any pure-Python
program" (post RFC 0020 / 0021) and "**WeavePy can run any
program that compiles against CPython**". After this RFC lands:

- WeavePy ships a public C-API surface in `crates/weavepy-capi`
  that mirrors the documented CPython 3.13 `Py_LIMITED_API`
  subset, plus the small set of unstable helpers (`PyType_FromSpec`,
  `PyCapsule`, the buffer protocol minimum) that idiomatic
  extensions reach for in practice. ~250 functions / ~30 statics
  in total.
- The C-API ships as a real, dlopen-able **`Python.h`**
  (`crates/weavepy-capi/include/Python.h`) that:
  1. Defines `PyObject`, `PyTypeObject`, `PyMethodDef`,
     `PyModuleDef`, `PyType_Spec` / `PyType_Slot`, `Py_buffer`,
     `PyCapsule_Destructor`, and the rest of the published shapes
     bit-for-bit identical to CPython's headers (so the same
     extension `.c` source compiles unchanged against either).
  2. Declares the full `Py_None` / `Py_True` / `Py_False` /
     `Py_NotImplemented` / `Py_Ellipsis` singleton statics.
  3. Declares the `PyExc_*` exception statics (`PyExc_TypeError`,
     `PyExc_ValueError`, etc.) the same way CPython does — as
     pointers exported by the host runtime.
  4. Defines the `PyMODINIT_FUNC` /
     `PyModuleDef_HEAD_INIT` / `Py_INCREF` / `Py_DECREF` macros
     so the extension build experience matches CPython's.
- A new `weavepy-capi` crate implements the surface in Rust:
  - `object.rs` — `PyObject` / `PyObjectBox` layout and the
    refcount bridge between the heap-boxed C-side handles and
    the `Rc<…>`-driven Rust side.
  - `singletons.rs` — static `Py_None` / `Py_True` / `Py_False` /
    `Py_NotImplemented` / `Py_Ellipsis` cells with immortal
    refcounts.
  - `types.rs` — `PyTypeObject` / `PyType_Spec` /
    `PyType_FromSpec` / `PyType_Ready`, plus the bridge that
    maps each `Rc<TypeObject>` to a stable `*mut PyTypeObject`.
  - `module.rs` — `PyModule_Create2`, `PyMethodDef` decoding,
    and the `BuiltinFn` adapter that turns a C
    `(self, args) -> PyObject*` into a Rust closure the
    interpreter can call.
  - `numbers.rs` / `strings.rs` / `containers.rs` — concrete
    constructors and accessors for `int` / `float` / `bool` /
    `complex` / `str` / `bytes` / `bytearray` / `list` /
    `tuple` / `dict` / `set` / `frozenset`.
  - `abstract_.rs` — the abstract-object protocol
    (`PyObject_GetAttr`, `PyNumber_Add`, `PySequence_GetItem`,
    `PyMapping_HasKey`, `PyIter_Next`, …).
  - `errors.rs` — pending-exception thread local;
    `PyErr_SetString` / `PyErr_Occurred` / `PyErr_Clear` /
    `PyErr_Fetch` / `PyErr_Restore`; the `PyExc_*` statics.
  - `memory.rs` — `PyMem_Malloc` / `PyMem_Free` and the
    `PyObject_Malloc` aliases.
  - `lifecycle.rs` — `Py_Initialize` / `Py_Finalize` stubs and
    the GIL no-ops (`PyGILState_Ensure` / `_Release`).
  - `capsule.rs` / `buffer.rs` / `slice.rs` — auxiliary
    protocols.
  - `argparse.rs` + `varargs.c` — non-variadic Rust helpers
    plus a tiny C shim that walks the `va_list` for
    `PyArg_ParseTuple`, `Py_BuildValue`, `PyErr_Format`,
    `PyUnicode_FromFormat`, `PyTuple_Pack`,
    `PyObject_CallFunction`, `PyObject_CallMethod`, etc.
    (Stable Rust does not support receiving `va_list`.)
  - `loader.rs` — the dlopen front end. Loads the extension's
    `.so` / `.dylib` / `.pyd`, locates `PyInit_<name>`, calls
    it under an active interpreter context, and returns the
    resulting `Object::Module`.
  - `interp.rs` — thread-local "active interpreter" handle so
    C-API functions can call back into the running VM
    (`PyImport_ImportModule`, `PyObject_Call`, …).
  - `ffi.rs` — the small set of typedefs / opaque pointer
    aliases shared between modules.
  - `force_link_table.rs` — a `#[used]` static array that
    references every `extern "C"` entry on the surface, so the
    linker doesn't dead-strip them out of the host binary
    when nothing in pure-Rust code references them.
- The existing `weavepy-vm` import machinery learns about a
  process-global **extension loader hook**
  (`weavepy_vm::ext_loader`). When the import system encounters
  an extension `.so`, it calls the hook to materialise an
  `Object::Module`. The top-level `weavepy` crate installs the
  C-API loader at startup so tests, the REPL, and `weavepy
  script.py` all transparently load extensions.
- A real, working **C extension** ships as a fixture:
  `tests/capi_ext/_smalltest.c`. It uses only documented
  C-API surface (`PyArg_ParseTuple`, `PyLong_FromLong`,
  `PyType_FromSpec`, `PyMethodDef`, `PyModule_AddObject`, …)
  and exposes:
  - `add(a, b)` / `concat(s1, s2)` / `make_pair(x, y)` —
    function bodies that exercise `PyArg_ParseTuple`,
    `Py_BuildValue`, `PyMem_Malloc` / `Free`, and `PyLong_FromLong`.
  - `oops(msg)` — raises `ValueError`, exercising the
    `PyErr_SetString` / `PyExc_ValueError` path.
  - `Counter` class — a `PyType_FromSpec`-built type with a
    `tick()` method.
  - `VERSION` (str) and `MAGIC` (int) module constants.
- `crates/weavepy-capi/build.rs` compiles `varargs.c` into a
  static archive that links into the host binary. When a C
  compiler is available, the same `build.rs` also compiles
  `_smalltest.c` into a real shared library, sets
  `WEAVEPY_CAPI_TEST_EXTENSION` to its path, and lets the
  Rust integration tests dlopen it end-to-end.
- A Rust integration test (`crates/weavepy-capi/tests/capi_loader.rs`)
  drives the C extension through the running interpreter:
  it builds an `Interpreter`, dlopen's `_smalltest.so`, calls
  `add(2, 3)`, `concat("foo", "bar")`, and `oops("nope")`,
  and asserts the results round-trip correctly across the FFI
  boundary including pending-exception propagation.

The combination delivers what `README.md` calls a "C-extension
ecosystem" baseline: any extension that uses the documented
C-API surface and avoids the small set of CPython internals we
deliberately don't expose (private `_PyXxx` functions, the
ceval frame layout, the GIL-as-mutex model) compiles unchanged
and runs unchanged on WeavePy.

## Motivation

After RFC 0020, every "drop-in for pure-Python code" workflow
worked: REPL, `pip install` for source distributions, the
standard library, third-party pure-Python wheels. After RFC
0021, those workflows ran at competitive speed.

What still didn't work: anything compiled. Specifically:

- **`numpy`, `pandas`, `lxml`, `cryptography`, `pyyaml`,
  `psycopg`, `Pillow`, …** — every "real-world Python"
  package ships at least one C extension (or Cython, which
  compiles to C). These were 100% non-functional on WeavePy.
- **`pip install <c-ext-package>`** would download the wheel,
  unpack the `.so`, and then fail at import time because
  WeavePy had no `Python.h` to compile against and no dlopen
  loader to call.
- **Cython, pybind11, nanobind, PyO3** — the major bindings
  generators all emit C code that includes `Python.h` and
  links symbolically against the running interpreter. None
  could target WeavePy.
- **`ctypes`** — works without a C extension layer (it dlopens
  arbitrary `.dylib`s and walks structs), but everything
  *built on top of* `ctypes` for performance (`numpy`,
  `cffi` itself, …) needs the actual C-API.

The goal of this RFC is to make every one of these work
unchanged. We're not inventing a new C-API; we're shipping
the existing CPython 3.13 one, byte-for-byte where it has
publicly-documented layouts and semantics.

The design pressure is **compatibility, not novelty**. Every
deviation from CPython's surface is a paper cut waiting to
happen for a downstream package. So we mirror:

- The exact `Python.h` macros (`PyMODINIT_FUNC`,
  `PyModuleDef_HEAD_INIT`, `Py_INCREF`, `Py_TYPE`, …) that
  Cython / pybind11 / hand-written extensions reach for.
- The exact `PyMethodDef` / `PyModuleDef` / `PyType_Spec`
  layouts and flag constants (`METH_VARARGS`, `METH_NOARGS`,
  `Py_TPFLAGS_DEFAULT`, …).
- The exact `PyObject *` ABI: a pointer to a struct whose
  first two fields are `Py_ssize_t ob_refcnt` and
  `PyTypeObject *ob_type`. Extensions that walk these fields
  by hand (it happens) keep working.
- The exact calling-convention semantics for refcounts: who
  steals, who borrows, who returns a new reference. Documented
  per-function on the Rust side; matched to CPython's docs.
- The exact `PyArg_ParseTuple` / `Py_BuildValue` format string
  alphabet: `i`, `l`, `L`, `n`, `f`, `d`, `s`, `s#`, `y`, `y#`,
  `O`, `O!`, `O&`, `p`, `(...)`, `[...]`, `{...}`, `|`, `:`,
  `;`, `*`, `&`. Same parser; same edge cases.

The only place we **deliberately** diverge from CPython is the
GIL: WeavePy is single-threaded by design (RFC 0016 covers the
free-threaded story), so `PyGILState_Ensure` / `_Release`
return canned states and never block. This matches CPython's
own behaviour when the GIL is statically disabled (`Py_NOGIL`
builds) and is what every modern extension already tolerates.

## Design — overview

The crate dependency graph after this RFC:

```
weavepy (top-level binary crate)
  ├── weavepy-cli
  ├── weavepy-vm           ←  knows about the *hook* (no capi dep)
  ├── weavepy-capi         ←  uses weavepy-vm; installs the loader hook
  └── weavepy-compiler / weavepy-parser / weavepy-lexer / …
```

`weavepy-vm` has no compile-time dependency on `weavepy-capi`.
The connection is a runtime hook: `weavepy_vm::ext_loader` exposes
a process-global slot that the `weavepy-capi` loader registers
into. The top-level `weavepy` crate calls `install_capi_loader()`
on its first run, after which any import that resolves to a
`.so` / `.dylib` / `.pyd` flows through the C-API loader. This
direction-of-arrows matters: the VM crate stays free of FFI and
`libloading`, so it remains usable as a pure-Rust dependency
(e.g. for the WASM tier in RFC 0024).

End-to-end import flow when `import _smalltest` runs:

```
import _smalltest
        │
        ▼
Interpreter::import_path
        │  (RFC 0012 — module finder)
        ▼
finds tests/capi_ext/_smalltest.so
        │
        ▼
ext_loader::current_extension_loader()  ← weavepy-vm side of hook
        │
        ▼
weavepy_capi::loader::load_extension_module
        │  · libloading::Library::new(path)
        │  · lib.get(b"PyInit__smalltest")
        │  · enter_extension_call(interp, |_| init())
        │
        ▼
PyInit__smalltest  (in C)
        │
        ▼
PyModule_Create2(&_smalltest_def, …)   ← weavepy-capi
        │
        ▼
returns a *mut PyObject (boxed Object::Module)
        │
        ▼
weavepy_capi::object::clone_object
        │
        ▼
Object::Module(Rc<PyModule { name, dict }>)
        │
        ▼
sys.modules["_smalltest"] = module
```

The orthogonal data flow when the C extension calls back into
the VM (`PyImport_ImportModule`, `PyObject_Call`, …):

```
C-API entry (e.g. PyImport_ImportModule)
        │
        ▼
interp::with_active(|ctx| { let interp = &mut *ctx.interp; … })
        │
        ▼
Interpreter::import_path / Interpreter::call_object / …
        │
        ▼
back into Rust-side machinery — frames, dict lookups, etc.
```

`with_active` reads the thread-local `ACTIVE` cell that
`enter_extension_call` writes before invoking C code. The cell
holds a raw `*mut Interpreter`; it's safe because:

1. The interpreter outlives the dlopen call by construction
   (the extension loader holds a `&mut Interpreter`).
2. WeavePy is single-threaded; there are no concurrent
   readers of the cell.
3. We re-enter the VM only via `with_active`, which the
   C-API uses uniformly.

Future RFCs that add real threads (RFC 0016) will replace the
thread-local with a per-interpreter slot and add explicit
re-entrancy guards.

## Design — `Python.h`

`crates/weavepy-capi/include/Python.h` is what an extension's
build script (`setup.py build_ext`, `pyproject.toml`,
hand-rolled CMake, …) ultimately compiles against. It must
look like CPython's header to the byte for the bits that
extensions actually touch. The file is hand-written rather
than auto-generated for two reasons:

1. CPython's headers transitively include ~70 sub-headers
   (`object.h`, `pyport.h`, `pymem.h`, …); pulling that in
   verbatim would carry a lot of incidental complexity and
   would exhibit anti-features (build-time platform probes,
   private struct fields). We want the public surface only.
2. The header's role in our build is also to advertise WeavePy's
   compatibility level: a quick `grep` against this file
   answers "does WeavePy support `PyType_FromSpecWithBases`?".

The header contains, in order:

- A guard `#ifndef Py_PYTHON_H` and the standard `extern "C"`
  brace for C++ callers.
- The fundamental typedefs: `Py_ssize_t`, `Py_hash_t`,
  `Py_complex`, `Py_buffer`, `PyCFunction`,
  `PyCFunctionWithKeywords`, `PyCapsule_Destructor`,
  `Py_UCS{1,2,4}`, `Py_UNICODE`.
- The opaque `_object` / `_typeobject` declarations and the
  `PyObject` / `PyVarObject` / `PyTypeObject` typedefs.
- `PyObject` struct layout — `Py_ssize_t ob_refcnt`,
  `PyTypeObject *ob_type`. This is `PEP 703` shape, *not*
  the 3.13 immortal-bit shape, because the immortal bit lives
  inside the refcount range we already use (immortals get
  `Py_ssize_t::MAX / 2 - 1`).
- `PyVarObject` adds `Py_ssize_t ob_size`.
- `PyTypeObject` — the public layout. We expose `tp_name`,
  `tp_basicsize`, `tp_itemsize`, `tp_flags`, plus a `bridge`
  field (private to WeavePy; ignored by extensions). The
  header gates the slot fields the same way CPython does:
  available unconditionally because we don't ship a
  `Py_LIMITED_API` mode.
- `PyMethodDef` — the four-field shape:
  `const char *ml_name`, `PyCFunction ml_meth`,
  `int ml_flags`, `const char *ml_doc`.
- `PyModuleDef_Base` / `PyModuleDef` and the
  `PyModuleDef_HEAD_INIT` macro:
  ```c
  #define PyModuleDef_HEAD_INIT \
      { { 1, NULL }, NULL, 0, NULL },
  ```
  The trailing comma matters: CPython's macro produces a
  comma so that the next field of the surrounding initializer
  works without manual punctuation; downstream extension
  source assumes this and breaks subtly without it.
- `PyType_Spec` / `PyType_Slot` and the `Py_tp_*` slot ID
  constants (`Py_tp_doc`, `Py_tp_methods`, `Py_tp_init`, …).
  Extensions use `PyType_FromSpec(&spec)` to register heap
  types without writing out a full `PyTypeObject` literal.
- The `METH_*` flag constants: `METH_VARARGS`, `METH_KEYWORDS`,
  `METH_NOARGS`, `METH_O`, `METH_CLASS`, `METH_STATIC`,
  `METH_COEXIST`, `METH_FASTCALL`, `METH_METHOD`.
- The `Py_TPFLAGS_*` flag constants (basic / sequence /
  mapping / default).
- Singleton declarations:
  ```c
  extern PyObject _Py_NoneStruct;
  #define Py_None (&_Py_NoneStruct)
  extern PyObject _Py_TrueStruct;
  #define Py_True (&_Py_TrueStruct)
  /* … */
  ```
- Exception-class declarations:
  ```c
  extern PyObject *PyExc_BaseException;
  extern PyObject *PyExc_TypeError;
  extern PyObject *PyExc_ValueError;
  /* …roughly 35 of these… */
  ```
  Note the level of indirection: `PyExc_TypeError` is itself
  a pointer (so `PyErr_SetString(PyExc_TypeError, …)` is
  passing a pointer-to-pointer-of-PyObject). This mirrors
  CPython exactly.
- Function prototypes for the full surface: `Py_IncRef`,
  `Py_DecRef`, `PyLong_FromLong`, `PyDict_New`, `PyType_FromSpec`,
  `PyArg_ParseTuple`, `Py_BuildValue`, `PyImport_ImportModule`,
  …
- The user-visible macros:
  ```c
  #define Py_INCREF(o) Py_IncRef((PyObject *)(o))
  #define Py_DECREF(o) Py_DecRef((PyObject *)(o))
  #define Py_XINCREF(o) do { if (o) Py_INCREF(o); } while (0)
  #define Py_XDECREF(o) do { if (o) Py_DECREF(o); } while (0)
  #define Py_TYPE(o)    (((PyObject *)(o))->ob_type)
  #define Py_REFCNT(o)  (((PyObject *)(o))->ob_refcnt)
  #define Py_SIZE(o)    (((PyVarObject *)(o))->ob_size)
  ```
- The `PyMODINIT_FUNC` macro — the platform-aware
  `__attribute__((visibility("default")))` /
  `__declspec(dllexport)` decorator that marks the
  `PyInit_<modname>` entry point as exported from the
  shared library:
  ```c
  #if defined(_WIN32)
  #  define PyMODINIT_FUNC __declspec(dllexport) PyObject *
  #else
  #  define PyMODINIT_FUNC __attribute__((visibility("default"))) PyObject *
  #endif
  ```
- A trailing `#endif /* Py_PYTHON_H */` plus the closing
  `extern "C"` brace.

The header is small enough (~600 lines) that it ships
in-tree rather than via build-time generation. Future
extensions to the surface land here as additional
declarations.

## Design — `PyObject` and the refcount bridge

Every value the C side holds is a heap-allocated
`PyObjectBox`:

```rust
#[repr(C)]
pub struct PyObject {
    pub ob_refcnt: PySsizeT,
    pub ob_type: *mut PyTypeObject,
}

#[repr(C)]
pub struct PyObjectBox {
    pub head: PyObject,           // ABI-visible prefix
    pub payload: PayloadCell,     // private suffix
}

pub struct PayloadCell {
    pub obj: Object,                                          // the wrapped Rust value
    pub user_data: *mut c_void,                               // capsules / module state
    pub destructor: Option<unsafe extern "C" fn(*mut PyObject)>,
}
```

The `#[repr(C)]` ensures the `head` field starts at offset 0,
so a `*mut PyObjectBox` cast to `*mut PyObject` is bit-for-bit
the address C expects. `into_owned(obj)` constructs a fresh
box, sets `ob_refcnt = 1`, picks the `ob_type` from the static
type table (RFC 0022 §"types" below), and hands the
`*mut PyObject` to the caller. `clone_object(p)` reads the box
back into a `Object::*` value the Rust side can clone freely
through `Rc`.

**Singletons** (`Py_None`, `Py_True`, `Py_False`,
`Py_NotImplemented`, `Py_Ellipsis`) live in `static` storage
with refcount `IMMORTAL_REFCNT = isize::MAX / 2 - 1`. The
`Py_IncRef` / `Py_DecRef` paths short-circuit when they see
the immortal sentinel value, so passing a static pointer
through the refcount churn is a no-op:

```rust
#[no_mangle]
pub unsafe extern "C" fn Py_IncRef(op: *mut PyObject) {
    if op.is_null() { return; }
    let head = &mut *op;
    if head.ob_refcnt >= IMMORTAL_REFCNT { return; }
    head.ob_refcnt += 1;
}
```

`clone_object` recognises the singleton structs by raw
pointer-eq before attempting to read the box layout (which
the singletons don't have): the static storage is exactly a
`PyObject` head, not a full `PyObjectBox`. Forgetting this
check is undefined behaviour; the current implementation
short-circuits all five singletons explicitly.

**Static type pointers** are also immortal. Each WeavePy
built-in type (`int`, `str`, `list`, `dict`, …) gets a
static `PyTypeObjectBox` whose `bridge` field points at a
`Box<Rc<TypeObject>>`. `clone_object` checks the
incoming pointer's `ob_type` against the metaclass
(`PyType_Type`) before trusting that the pointer is itself a
type — without that check, an arbitrary `PyObjectBox` could
be misinterpreted as a `PyTypeObjectBox` and we'd dereference
random payload bytes as `bridge`. (We hit exactly that bug
during bring-up; the fix is in `object.rs::clone_object`.)

**Lifetimes**. The C side owns its references through
`ob_refcnt`. When the count reaches zero, `Py_DecRef` calls
`free_box`, which moves the box back into a `Box<PyObjectBox>`
and drops it. Dropping the box drops the `Object` payload,
which decrements any `Rc<…>` it carried; if those drop to
zero, the underlying allocation goes away too.

This means a `*mut PyObject` and a `Object::*` value can both
keep memory alive simultaneously, and dropping one is safe
without affecting the other. The price is two layers of
refcounting (C-side `ob_refcnt` and Rust-side `Rc::strong`)
on every value; we judged this acceptable for the foundation,
and there's a plan to collapse the two layers in a future RFC
once we have benchmarks that justify the complexity.

## Design — types

`PyType_FromSpec(spec)` is the modern way to register a
heap-allocated type from C. It takes a `PyType_Spec` struct
that names the type, gives its in-memory size, and lists the
slots (`Py_tp_doc`, `Py_tp_methods`, `Py_tp_init`, …). The
C side never gets to see the underlying `Rc<TypeObject>`;
that's our private representation.

Implementation:

1. Allocate a fresh `PyTypeObjectBox` on the heap. Its
   `head.ob_type` points at the `PyType_Type` metaclass
   singleton (so a future `clone_object(ty)` recognises it).
2. Fill in `tp_name`, `tp_basicsize`, `tp_itemsize`,
   `tp_flags` from the spec.
3. Walk the slot table:
   - `Py_tp_doc` → cache the docstring on the Rust type.
   - `Py_tp_methods` → decode the `PyMethodDef[]` array via
     `module::collect_methods`, wrap each entry in a
     `BuiltinFn` via `module::wrap_c_function`, and stash
     them in the type's method dict.
   - `Py_tp_init` / `Py_tp_new` / `Py_tp_dealloc` /
     `Py_tp_repr` / `Py_tp_str` → register a Rust-side
     trampoline that invokes the C function with a freshly
     boxed `PyObject` and translates the result back. The
     trampoline lives in `types.rs::dispatch_c_slot`.
4. Construct an `Rc<TypeObject>` describing the new type and
   store it in the box's `bridge` field. Subsequent
   `clone_object` calls return `Object::Type(rc.clone())`,
   so the rest of the VM treats it like any other class.
5. Register the type in the global static `TYPE_TABLE` so
   future `type_for_object(obj)` lookups find it (used when
   building a fresh `PyObject *` for an `Object::Instance`
   of this type).

`PyType_FromSpecWithBases` is the same path, with one
adjustment: the `bases` parameter is allowed to be either
a tuple of types or a single `Object::Type`. The CPython
docs are fuzzy on this; `PyType_FromSpecWithBases(spec, ty)`
appears in the wild with both shapes. We accept both.

`PyType_Ready` is a no-op for us. CPython uses it to
recompute the MRO and patch slot caches; our types are
already fully constructed by the time `PyType_FromSpec`
returns. The header still exposes `PyType_Ready` so existing
extension source compiles, and the implementation returns 0.

The type table itself is a `RwLock<Vec<TypeEntry>>` (we use
`std::sync` even though WeavePy is single-threaded today,
because it's free in single-threaded use and gives us a
non-deadlocking shape if RFC 0016 lands later).

## Design — modules and `PyMethodDef`

`PyModule_Create2(def, api)` walks the `PyModuleDef`:

1. Decode `m_name` / `m_doc` into a `String`.
2. Build a fresh module dict, populated with the standard
   dunders: `__name__`, `__doc__`, `__package__`,
   `__loader__`, `__spec__`.
3. If `m_methods` is non-null, decode each `PyMethodDef`
   via `collect_methods` and install the resulting
   `BuiltinFn`s into the dict.
4. Box up an `Object::Module(Rc<PyModule { name, dict, … }>)`
   and return a `*mut PyObject` pointing at the new box.

The `BuiltinFn` adapter (`module.rs::wrap_c_function`) is
the workhorse for every C function called by the
interpreter. The closure it produces:

1. Asserts the C-API global state is initialised
   (`interp::ensure_initialised`).
2. Clears any pending exception (`errors::clear_thread_local`).
3. Builds the `self` pointer:
   - For free functions: `self_ptr = Py_None`.
   - For bound methods: `self_ptr = into_owned(receiver)`.
4. Builds the args object based on the calling convention:
   - `METH_NOARGS` → `(self, Py_None)`.
   - `METH_O` → `(self, into_owned(args[0]))`.
   - `METH_VARARGS` → `(self, into_owned(tuple(args)))`.
   - `METH_VARARGS | METH_KEYWORDS` → adds a kwargs dict
     and casts the function pointer to the 3-arg shape.
5. Calls the C function. On NULL return, fetches the
   pending exception (`errors::take_pending`) and turns it
   into a `RuntimeError::PyException`; on non-NULL,
   `clone_object`s the result into a Rust `Object`.
6. Decrefs the temporaries and returns.

The closure is stored in `Object::Builtin(Rc<BuiltinFn { name, call }>)`.
The interpreter's call dispatch (`crates/weavepy-vm/src/lib.rs`'s
`Object::Builtin(b) => (b.call)(args)`) sees the wrapped
closure exactly the same way it sees a Rust-defined
builtin like `print` or `abs`. C functions are first-class
citizens in the VM call graph after this RFC.

## Design — variadic helpers (the C shim)

`PyArg_ParseTuple(args, fmt, ...)`, `Py_BuildValue(fmt, ...)`,
`PyErr_Format(ty, fmt, ...)`, and friends all take a
`...` parameter and walk the `va_list` to extract typed
arguments. Stable Rust does not support receiving a `va_list`
(the unstable `c_variadic` feature exists but isn't on
nightly's stable track), so we split the work:

```
extension.c
    │ calls PyArg_ParseTuple(args, "ll", &a, &b)
    ▼
varargs.c (compiled via cc::Build in build.rs)
    │ walks the format string with va_arg
    │ pulls each destination off the va_list
    │ calls non-variadic Rust helpers with the dest pointer
    ▼
argparse.rs
    │ _WeavePy_Arg_Long(arg, dest) etc.
    │ reads the PyObject * → Rust Object
    │ writes the typed value through the dest pointer
    ▼
returns 0/1 to varargs.c, which returns to the extension
```

`varargs.c` is ~600 lines of "for each format unit, va_arg
the destination pointer, call into Rust". It supports the
documented format alphabet (modulo the deliberately-omitted
units; see "Drawbacks" below).

The shim handles four families:

- **`PyArg_*`** — the parse side. `PyArg_ParseTuple`,
  `PyArg_ParseTupleAndKeywords`, `PyArg_VaParse`,
  `PyArg_UnpackTuple`, `PyArg_Parse`. They share a single
  `parse_args_from(args, fmt, ap)` core.
- **`Py_BuildValue`** — the build side. Walks `fmt`, calls
  `_WeavePy_Build_FromI64` / `_FromDouble` / `_FromString` /
  `_FromBytesAndSize` / `_TupleFromArray` / `_ListFromArray` /
  `_DictFromArrays` for each unit. Top-level units fold into
  a tuple if there's more than one.
- **`PyTuple_Pack(n, ...)`** — convenience that walks `n`
  PyObject pointers off the va_list into an array, then
  delegates to `_WeavePy_TuplePackFromArray`.
- **`Py_*Format` / `PyErr_Format` / `PyUnicode_FromFormat`** —
  printf-style. We use `vsnprintf` on the C side (because
  Rust's `format!` doesn't speak C `printf` syntax), then
  hand the resulting C string to a Rust helper that wraps it
  in a `PyObject` or sets it as the pending exception
  message.

The shim deliberately does *not* call back into the VM; it
only touches the non-VM Rust helpers in `argparse.rs`. This
keeps the shim simple and avoids a circular dependency from
`varargs.c` (compiled by `cc::Build`) to the rest of the
crate (compiled by rustc).

The `cc::Build` invocation in `build.rs`:

```rust
let mut build = cc::Build::new();
build
    .file(manifest.join("src/varargs.c"))
    .include(manifest.join("include"))
    .flag_if_supported("-Wno-format-nonliteral")
    .flag_if_supported("-Wno-incompatible-pointer-types")
    .flag_if_supported("-Wno-pointer-sign");
build.compile("weavepy_capi_varargs");
```

The static archive `libweavepy_capi_varargs.a` ends up linked
into anything that depends on `weavepy-capi`. The `force_link`
mechanism (next section) keeps the symbols from being
dead-stripped.

## Design — symbol export and `force_link`

Mach-O / ELF binaries dead-strip symbols that nothing else
references. By default, a `cargo test` binary built against
`weavepy-capi` would strip *every* `#[no_mangle] extern "C"`
symbol from the C-API surface, because no Rust code in the
test binary calls them — they're only called from C, and
the C side is loaded later via `dlopen`.

The fix is a `#[used] static` table that takes the address
of every public C-API entry, in
`crates/weavepy-capi/src/force_link_table.rs`:

```rust
#[used]
static FORCE_LINK: &[FnPtr] = &[
    addr!(numbers::PyLong_FromLong),
    addr!(numbers::PyLong_FromLongLong),
    addr!(strings::PyUnicode_FromString),
    /* … ~270 entries total … */
];
```

`#[used]` tells the compiler "don't optimise this static away";
the chain of pointer-references inside forces the linker to
keep the targeted symbols. `force_link()` is a public no-op
function that touches the table; embedders that build with
LTO must call it once at startup so the LTO pass sees a use.

A second wrinkle: `varargs.c` defines `PyArg_ParseTuple`,
`Py_BuildValue`, etc. in C, not Rust. Those symbols live in
the static archive `libweavepy_capi_varargs.a`. They share
the same dead-stripping fate, and `#[no_mangle]` doesn't help
because they're not Rust definitions. So `force_link_table.rs`
also `extern "C"`-declares each of these and includes them in
the table:

```rust
extern "C" {
    fn PyArg_ParseTuple(args: *mut PyObject, fmt: *const c_char, ...) -> c_int;
    fn Py_BuildValue(fmt: *const c_char, ...) -> *mut PyObject;
    /* … */
}
```

Without these references, the test binary doesn't pull
`varargs.o` into its dynamic symbol table, and the dlopen'd
extension's call to `PyArg_ParseTuple` resolves to a stub
that segfaults the moment a non-zero address is required.

`force_link` is invoked by:

- `weavepy_capi::loader::load_extension_module` (so any
  dlopen automatically primes the symbol table).
- `weavepy::install_capi_loader` (so the top-level binary
  primes it on first run).
- The integration tests in `crates/weavepy-capi/tests/capi_loader.rs`.

This is the same pattern CPython uses internally for its
`PyAPI_FUNC` set, except CPython relies on the fact that the
main interpreter binary calls each of those functions
somewhere along its own startup path; we need the explicit
table because WeavePy's startup path is pure-Rust and
doesn't naturally touch the C surface.

## Design — error handling

CPython tracks the "current exception" in a thread-local set
of three slots (`type`, `value`, `traceback`). C-API
functions return NULL to indicate failure and populate those
slots; the caller checks `PyErr_Occurred()` and either
propagates or recovers.

We mirror this with a thread-local `RefCell<Option<PendingError>>`
in `errors.rs`. The shape:

```rust
pub struct PendingError {
    pub class: Option<Rc<TypeObject>>,  // PyExc_TypeError, …
    pub value: Object,                   // string or arbitrary object
    pub traceback: Object,               // None for now
}
```

The public API:

- `PyErr_SetString(exc, msg)` — installs a new pending error.
- `PyErr_SetObject(exc, value)` — same, but with a non-string
  value.
- `PyErr_Occurred()` — returns the class pointer if a pending
  error exists, NULL otherwise.
- `PyErr_Clear()` — clears the slot.
- `PyErr_Fetch(*p_type, *p_value, *p_traceback)` — pops the
  slot into three out-parameters.
- `PyErr_Restore(type, value, traceback)` — pushes them back.
- `PyErr_NewException(name, base, dict)` — registers a new
  exception class in the runtime.
- `PyErr_BadArgument` / `_BadInternalCall` / `_NoMemory` /
  `_WarnEx` / `_NormalizeException` / `_ExceptionMatches` —
  the convenience layer.

The `PyExc_*` statics are `*mut PyObject` pointers
initialised at first use to point at the corresponding
WeavePy `TypeObject`. The lazy initialisation is needed
because `weavepy_vm::builtin_types::builtin_types()` builds
the static type table on the first call into the VM, and the
C-API can be touched before that point (e.g. by
`force_link`); `errors::ensure_exc_statics` resolves the
chicken-and-egg without re-entering the VM.

When the wrapped C function (called through a `BuiltinFn`
closure — see "modules" above) returns NULL, the closure
calls `take_pending` to drain the slot and converts the
error into a `RuntimeError::PyException` carrying the same
class and message. The interpreter's existing exception
machinery picks it up unchanged.

## Design — extension loader

`crates/weavepy-capi/src/loader.rs` is the dlopen front end.
The signature:

```rust
pub fn load_extension_module(
    interp: *mut Interpreter,
    path: &Path,
    name: &str,
) -> Result<Object, LoadError>;
```

Steps:

1. `force_link()` to ensure no symbols have been stripped.
2. `unsafe { libloading::Library::new(path) }` to open the
   shared object. Records the library in a process-global
   list so it isn't unloaded prematurely; CPython does the
   same.
3. Compute the entry-point name. CPython's rules:
   - For a module named `_smalltest`: look for
     `PyInit__smalltest`.
   - For a multi-phase init: look for `_PyInit_<name>` first,
     then fall through. We currently support single-phase
     init only; multi-phase init is on the future-work list.
4. `lib.get(b"PyInit__smalltest\0")` to look up the symbol.
   Errors here become `LoadError::MissingEntryPoint`.
5. `interp::enter_extension_call(interp, |_| init())` to
   run the entry point under an active interpreter context.
   The C side is now free to call `PyImport_ImportModule`,
   `PyObject_Call`, etc. and they'll route back to the
   live interpreter.
6. The entry point returns a `*mut PyObject` pointing at the
   newly-built module. We `clone_object` it into an
   `Object::Module`, then `Py_DecRef` the C-side handle.
7. Return the module to the caller.

Errors at any step are wrapped in a `LoadError` enum
(`thiserror::Error`). The variants:
`Library`, `MissingEntryPoint`, `InitFailed { class, msg }`,
`InitReturnedNull`, `InitReturnedNonModule`, `NoActiveInterpreter`.

The loader is hooked into the VM via
`weavepy_vm::ext_loader`. The VM crate exposes a process-global
slot:

```rust
pub static EXT_LOADER: Mutex<Option<Box<dyn ExtensionLoader>>> = …;

pub trait ExtensionLoader {
    fn load(&self, interp: *mut Interpreter, path: &Path, name: &str)
        -> Result<Object, RuntimeError>;
}
```

The top-level `weavepy` crate registers a concrete loader in
`install_capi_loader()` that delegates to
`weavepy_capi::loader::load_extension_module`. This call
happens from `run_source_with_options` and the REPL, before
any user code runs.

The VM's `import.rs` (RFC 0012) calls
`ext_loader::current_extension_loader()` when it encounters
a `.so` / `.dylib` / `.pyd` in the search path. If no loader
is installed, the VM raises `ImportError("C extension support
not available in this build")` — so a hypothetical "VM-only
WASM build" without `weavepy-capi` still parses code and
errors cleanly when an extension would be needed.

## Design — `_smalltest.c` fixture

The test extension `tests/capi_ext/_smalltest.c` exists for
two reasons:

1. **It's a real C extension that compiles unchanged against
   either CPython 3.13 or WeavePy.** That property is the
   whole point of this RFC. If we can't build the same
   `.c` file with the same `clang -I…/Python.h` invocation
   against both interpreters, we've failed.
2. **It exercises every shape of the C-API surface in a
   minimal fashion.** `PyArg_ParseTuple` of various format
   strings (`"ll"`, `"ss"`, `"OO"`, `"O"`, `"s"`),
   `Py_BuildValue` of the `(OO)` tuple shape, an exception
   raise (`PyErr_SetString` + `PyExc_ValueError`),
   `PyType_FromSpec` for a heap type, `PyMethodDef` with
   `METH_VARARGS` / `METH_NOARGS` / `METH_O`, module-level
   constants (`PyModule_AddIntConstant`,
   `PyModule_AddStringConstant`).

The extension is ~140 lines. It is intentionally not a
"feature test" — it's a smoke test. Comprehensive C-API
conformance comes from running CPython's own test suite
through the bridge, which RFC 0023 (forthcoming) ships.

## Design — build script integration

`crates/weavepy-capi/build.rs`:

```rust
fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let manifest = std::path::Path::new(&manifest_dir);
    let workspace_root = manifest.parent().unwrap().parent().unwrap();

    println!("cargo:rerun-if-changed=src/varargs.c");
    println!("cargo:rerun-if-changed=include/Python.h");

    // 1. Compile the variadic shim into a static archive.
    let mut build = cc::Build::new();
    build
        .file(manifest.join("src/varargs.c"))
        .include(manifest.join("include"))
        .flag_if_supported("-Wno-format-nonliteral")
        .flag_if_supported("-Wno-incompatible-pointer-types")
        .flag_if_supported("-Wno-pointer-sign");
    build.compile("weavepy_capi_varargs");

    // 2. If the test extension source exists, compile it
    //    into a real .so for the integration tests.
    let test_src = workspace_root.join("tests/capi_ext/_smalltest.c");
    if test_src.is_file() {
        let out_dir = std::env::var("OUT_DIR").unwrap();
        let dylib = std::path::Path::new(&out_dir)
            .join("capi_ext")
            .join("_smalltest.so");
        std::fs::create_dir_all(dylib.parent().unwrap()).ok();

        let mut cmd = std::process::Command::new("cc");
        cmd.arg("-shared")
            .arg("-fPIC")
            .arg("-fvisibility=default")
            .arg("-O0")
            .arg("-Wno-error")
            .arg(format!("-I{}", manifest.join("include").display()))
            .arg(&test_src)
            .arg("-o").arg(&dylib);
        if cfg!(target_os = "macos") {
            cmd.arg("-undefined").arg("dynamic_lookup");
        }
        match cmd.output() {
            Ok(out) if out.status.success() => {
                println!(
                    "cargo:rustc-env=WEAVEPY_CAPI_TEST_EXTENSION={}",
                    dylib.display()
                );
            }
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                println!("cargo:warning=test extension cc failed: {stderr}");
            }
            Err(err) => {
                println!("cargo:warning=could not run cc for test extension: {err}");
            }
        }
    }
}
```

Two outputs:

- A static archive `libweavepy_capi_varargs.a` that's linked
  into anything depending on `weavepy-capi`. This contains
  the `PyArg_ParseTuple` / `Py_BuildValue` family.
- A shared library `_smalltest.so` (when a C compiler is
  available), with its path exposed to test code via the
  `WEAVEPY_CAPI_TEST_EXTENSION` env var.

Building the test extension is best-effort: if `cc` isn't on
the path, the build emits a warning rather than failing, and
the integration tests skip via the env-var-missing branch.
This keeps `cargo build` green on minimal CI runners that
don't ship a compiler.

## Drawbacks

- **Surface debt.** ~250 functions is a lot to keep
  bug-compatible with CPython forever. The RFC text above
  enumerates ~50 of them; the rest follow the same shapes.
  Every CPython point release potentially adds, deprecates,
  or quietly changes the semantics of a few; we've signed up
  for tracking that.
- **C build dependency.** The test fixture and the variadic
  shim both need a working C compiler. We work around this
  for `cargo build` (best-effort), but the real-world
  workflow ("install a wheel and run") requires `cc` to be
  on the path on whatever machine compiles the wheel — same
  as CPython.
- **Refcounting overhead.** Every C ↔ Rust boundary crossing
  involves at least one `into_owned` (heap allocation) and
  one matching `Py_DecRef`. A future RFC can collapse this
  by sharing the `Object` allocation between the C-side box
  and the Rust-side `Rc<…>`, but that's a sizable refactor
  on top of an already-large RFC.
- **Limited variadic format support.** `PyArg_ParseTuple`
  documents ~30 format units; we ship the ~20 that matter
  for the common case. `e` (encoded string), `et` (encoded
  string with type tag), `w*` (writable buffer), and the
  `*Z` Unicode-or-None family are deferred. Same story for
  `Py_BuildValue`.
- **Stable Rust + C variadics.** We can't take a `va_list`
  in Rust on stable, hence the `varargs.c` shim. If
  `c_variadic` ever stabilises (or we move to nightly with
  pinned toolchains), the shim can collapse to plain Rust;
  we'd save ~600 lines of C and one cc-driven build step.
- **`PyModule_AddType` and metaclasses.** Our type table is
  flatter than CPython's; a custom metaclass passed to
  `PyType_FromSpec` mostly works but is reduced to a name —
  the metaclass itself isn't bound. Real metaclass-aware
  code is rare in practice; a future RFC can land it.
- **GIL semantics.** `PyGILState_Ensure` / `_Release` are
  no-ops. Extensions that *assume* the GIL is doing real
  serialization will see surprising results in a future
  multi-threaded WeavePy. This is the same drawback every
  no-GIL Python (`Py_NOGIL`) build has.
- **Symbol stripping risk.** If a downstream embedder builds
  against `weavepy-capi` with aggressive LTO and forgets to
  call `force_link()`, exposed symbols may still be stripped.
  We document this prominently in the crate docs and the
  README.
- **`pip wheel` integration is out of scope.** Wheels that
  bundle prebuilt `.so` files and reference CPython's exact
  ABI tag (`cp313-cp313-linux_x86_64`) won't work yet —
  we'd need an `wp313-wp313-…` ABI tag and matching
  pip-side glue. RFC 0023 covers this.

## Rationale and alternatives

**Alternative 1: stay pure-Rust forever.** Skip the C-API
entirely; tell users to wrap C libraries with PyO3-style
bindings or ctypes. We rejected this for the obvious reason:
the entire scientific-computing stack is unusable without it.
The README explicitly commits to "drop-in for any CPython
program," and that includes ones that import `numpy`.

**Alternative 2: re-export CPython's headers verbatim.**
We could vendor `Include/Python.h` from the CPython source
tree and patch out the bits we don't support. We rejected
this because (a) CPython's headers transitively pull in
~70 sub-headers full of internal and platform-specific bits,
(b) most of those headers reach into private `_Py*`
internals that we'd then have to either implement or stub
out one by one, and (c) WeavePy has already chosen public
shapes (e.g. our `bridge` field on `PyTypeObject`) that
diverge in private from CPython's. Hand-writing a clean
public-only header is a ~600-line one-time cost; the
maintenance burden of reverse-engineering CPython's whole
header tree would be far worse.

**Alternative 3: `Py_LIMITED_API`-only.** The "stable ABI"
subset is smaller and well-documented. We rejected this
because (a) most extensions in the wild target the
non-limited API (e.g. Cython doesn't use limited API by
default), (b) `PyType_FromSpec` is technically limited-API
but `Py_tp_*` slot IDs we need aren't all in the limited
set, and (c) the limited API forbids accessing
`ob_type` / `ob_refcnt` directly through the struct,
which would break the macros (`Py_TYPE`, `Py_REFCNT`)
extension code uses.

**Alternative 4: build `weavepy-capi` as a `cdylib` and
have extensions link against it explicitly** (rather than
relying on `-undefined dynamic_lookup`). This is the
cleanest model on Linux — no `force_link` games, the
linker resolves everything at extension-build time. It
would also work on macOS. We deferred it because (a) it
introduces an `rpath` / `install_name` step that complicates
the build matrix, (b) extensions built this way wouldn't
also work against CPython unchanged (which is the whole
point), and (c) `dynamic_lookup` is what CPython does on
both Linux and macOS in practice. We may revisit if
`force_link` proves brittle.

**Alternative 5: hot-build the variadic shim in Rust with
`asm!`.** The `va_list` ABI is well-documented per platform,
and we could implement `va_arg` in inline assembly. This
removes the C dependency. We rejected it because the asm
would need three different implementations (System V x86_64,
Apple ARM64, Windows x86_64), each one a non-trivial
correctness exercise. Two pages of C is a much smaller
maintenance footprint.

**Alternative 6: per-call interpreter context.** Instead of
a thread-local, pass a `*mut Interpreter` to every C-API
function. This requires changing the function signatures,
which would break ABI compatibility with CPython —
unacceptable. The thread-local is what CPython itself does;
we follow.

## Prior art

- **CPython 3.13** — *the* reference. Header layouts,
  function semantics, format-string alphabet all come from
  here. We track 3.13 specifically because it's the
  current major and the version Cython / pybind11 / etc.
  primarily target.
- **PyPy's `cpyext`** — PyPy's C-API emulation layer. Same
  architectural shape (boxed handles, lazy bridge to the
  underlying interpreter, dlopen loader), and a long list
  of compatibility quirks they discovered. Required reading
  for the future-work tier (see "Future work" below).
- **GraalPy** — implements the C-API on top of Truffle's
  native interface (`Sulong`). Different host, same
  destination.
- **MicroPython's `mpy_native_*`** — MicroPython exposes a
  much smaller surface but solves the dlopen-vs-static
  problem we're solving here. Their force-link table
  approach informed ours.
- **Stackless Python** — historical reference for
  preserving CPython ABI compatibility while changing the
  underlying runtime. Stackless made the same calculation:
  break the call stack, keep `Python.h`. We're doing the
  same with the GC / refcounting model.
- **`libpython.so` itself** — we copied the `PyAPI_FUNC`
  / `PyAPI_DATA` decoration pattern (visibility attributes
  on every public symbol), the `PyMODINIT_FUNC` macro
  shape, and the `PyExc_*` indirection (pointer-to-pointer)
  exactly.

## Unresolved questions

- **Multi-phase init (`PyModuleDef_Slot[]`).** We currently
  treat `PyModuleDef_Init` the same as `PyModule_Create`,
  which loses the slot table. Real-world Cython modules
  occasionally use multi-phase init; we'll likely need it
  in the next RFC.
- **`pip` ABI tag.** What does `pip install numpy` resolve
  to under WeavePy? `cp313` (and we satisfy that ABI by
  exposing `Python.h`) or a fresh `wp313` tag with its own
  wheel set? RFC 0023 should pick one and stick.
- **Sub-interpreters.** CPython 3.12+ ships per-interpreter
  GIL machinery. Our thread-local active interpreter
  doesn't compose with that; if we ever support sub-
  interpreters, the slot moves into the interpreter struct.
- **Object pool / arena-backed allocations.** Right now
  every `into_owned` is a fresh `Box::new`. CPython has a
  small-object allocator that's a big perf win. Future
  perf RFC.
- **Capsule destructors and shared state.** We support
  `PyCapsule_New(ptr, name, dtor)`, but the destructor is
  called from `Py_DecRef` rather than from a guaranteed
  process-exit hook. CPython's behaviour matches this in
  practice, but the docs are vague.
- **Buffer protocol coverage.** We ship `PyObject_GetBuffer`
  and `PyBuffer_Release` for `bytes` / `bytearray` /
  `memoryview` only. NumPy's full N-dimensional buffer
  protocol (strides, suboffsets, ndim) is the obvious
  next step.
- **Threading API stubs.** `PyThread_*` family is barely
  represented; same for `Py_AddPendingCall`. RFC 0016
  territory.
- **`PyConfig` / `PyPreConfig`.** CPython's modern
  initialisation API. We don't expose it; `Py_Initialize`
  is the only way in. RFC 0023.

## Future work

- **CPython test-suite bring-up (RFC 0023).** Run the
  upstream `Lib/test/test_capi*.py` suite through the
  bridge. Each failure is a tiny RFC 0022.x — a missing
  format unit, a slightly-wrong refcount, a pointer
  semantics mismatch. Our `_smalltest.c` is the
  proof-of-concept; the real conformance comes from CPython
  itself.
- **NumPy.** The single most important downstream package.
  Bringing up `numpy` will exercise the buffer protocol
  fully, the array iterator protocol, the descr / dtype
  type tower, and many of the format units we deferred.
  The first concrete milestone after this RFC.
- **Cython-generated modules.** Cython's runtime support
  module (`__Pyx_*`) reaches deep into private CPython.
  We'll need to either implement those private functions
  (against our internal ABI) or convince the Cython
  project to gate them behind a "minimal target" mode.
- **PyO3 cohabitation.** PyO3 is the natural way to write
  Rust extensions for CPython. WeavePy embedders shouldn't
  need to write C — they should be able to use PyO3
  unchanged. PyO3 currently doesn't know about WeavePy;
  upstream support is the long-term goal.
- **Wheel ABI tag (`wp313-wp313-…`).** New ABI tag,
  `pip` plumbing to recognise it, and a build mode in our
  `weavepy-cli` for bundling extensions into a wheel.
- **`Py_LIMITED_API` mode.** A future build flag that
  restricts the surface to the documented stable ABI.
  Useful for embedders who want a smaller compatibility
  burden; their extensions then work against any
  Python ≥ 3.2 build that supports the limited API.
- **Multi-phase init.** Decode `PyModuleDef.m_slots` and
  drive the `Py_mod_create` / `Py_mod_exec` slots in the
  documented order. Required for some Cython modules.
- **`PyConfig` / `PyPreConfig`.** Add modern interpreter-
  configuration objects, deprecate `Py_SetProgramName` /
  `Py_SetPythonHome`. CPython 3.13 uses these everywhere.
- **GIL semantics for free-threaded extensions.** Once
  RFC 0016 lands, `PyGILState_Ensure` becomes a real
  (per-interpreter) lock again, and we add the
  `Py_BEGIN_ALLOW_THREADS` macros. Extensions that release
  the GIL during long computations will then actually
  parallelise.
- **Object-allocator perf pass.** Replace `Box::new` for
  small Python objects with a slab allocator (CPython's
  `obmalloc.c` shape) so common allocations (`int`, small
  `tuple`) don't hit the system allocator.
- **Refcount collapse.** Share the `Object` storage
  between the `*mut PyObject` box and the `Rc<…>` clone.
  Eliminates the double-refcount cost on every C ↔ Rust
  boundary crossing.
- **`PyType_FromMetaclass`.** CPython 3.12's recent
  addition for metaclass-aware spec construction. We
  declare it but route to `PyType_FromSpec` today.
- **`PyContext` (PEP 567).** Required for full asyncio
  compatibility from C; we defer to the existing pure-Rust
  context-var implementation.
- **`PyObject_*` fast-call protocol.** CPython 3.7+
  vectorcall (`tp_vectorcall_offset`). Big perf win; we
  declare the flag but don't take the fast path yet.
- **Build wheels from C extensions on the fly.** A
  `cargo run -- pip install --build-isolation`-style
  fallback that compiles a sdist on the user's machine
  using our `Python.h`.

## Implementation status (post-merge)

| area                                | status      | notes                                                                       |
|-------------------------------------|-------------|-----------------------------------------------------------------------------|
| `weavepy-capi` crate scaffold       | ✅ done     | Cargo.toml, lib.rs, ~20 modules                                             |
| `Python.h` header                   | ✅ done     | ~600 lines, compiles unchanged against CPython 3.13 source                  |
| `PyObject` / `PyObjectBox`          | ✅ done     | refcount bridge, immortal singletons, type-object short-circuit             |
| `PyType_FromSpec` / `_FromSpecWithBases` | ✅ done | docstring, methods slots; `_FromMetaclass` declared, routes to `_FromSpec`  |
| `PyModule_Create2` + `PyMethodDef`  | ✅ done     | METH_VARARGS / METH_NOARGS / METH_O / METH_KEYWORDS                         |
| `PyArg_ParseTuple` / family         | ✅ done     | format alphabet: i I h b B l L q K Q n f d s s# y y# z z# O O! O& p U; `\|`/`:`/`;` |
| `Py_BuildValue` / `PyTuple_Pack`    | ✅ done     | tuples / lists / dicts / strings / numbers; one-vs-many top-level units      |
| `PyErr_*` + `PyExc_*` statics       | ✅ done     | thread-local pending-error slot; ~35 exception classes                       |
| `PyMem_*` / `PyObject_*` allocators | ✅ done     | Malloc / Free / Realloc / Calloc / Raw* aliases                              |
| `PyImport_ImportModule` / `_GetModule` / `_AddModule` | ✅ done | routes through active interpreter context                       |
| `PyCapsule_*`                       | ✅ done     | New / GetPointer / GetName / SetPointer / IsValid / destructors             |
| `Py_buffer` + `PyObject_GetBuffer`  | ✅ done     | bytes / bytearray / memoryview only                                          |
| `PySlice_*`                         | ✅ done     | New / Check                                                                  |
| `force_link_table.rs`               | ✅ done     | ~270 entries; covers Rust `#[no_mangle]` *and* C-shim symbols               |
| `loader.rs` + `ext_loader` hook     | ✅ done     | dlopen via `libloading`; PyInit\_\<name> dispatch; LoadError variants        |
| `varargs.c` shim + `cc::Build`      | ✅ done     | static archive `libweavepy_capi_varargs.a`                                  |
| `_smalltest.c` test fixture         | ✅ done     | exercises ParseTuple / BuildValue / FromSpec / SetString                    |
| `tests/capi_loader.rs` integration  | ✅ done     | 6 tests, all pass; skip-on-missing-cc semantics                              |
| Multi-phase init (`m_slots`)        | 🔜 deferred | RFC 0022.1                                                                  |
| Buffer protocol (N-dim, strides)    | 🔜 deferred | required for NumPy; RFC 0023                                                 |
| `Py_LIMITED_API` mode               | 🔜 deferred | future-work tier                                                             |
| Wheel ABI tag                       | 🔜 deferred | RFC 0023                                                                     |
| Vectorcall                          | 🔜 deferred | flag declared; perf RFC                                                       |
| PyO3 / Cython upstream              | 🔜 deferred | external dependency                                                          |
