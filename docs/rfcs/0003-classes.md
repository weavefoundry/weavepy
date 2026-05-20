# RFC 0003: Classes, the type system, and the data model

- **Status**: Accepted
- **Authors**: WeavePy authors
- **Created**: 2026-05-20
- **Tracking issue**: TBD

## Summary

Add Python's class system to the executable slice. After this RFC lands,
WeavePy can run user-defined classes — single and multiple inheritance,
methods (instance, class, and static), `__init__` / `__new__`, the
operator dunders (`__add__`, `__eq__`, `__lt__`, …), the iterator
protocol (`__iter__` / `__next__`), the container dunders
(`__len__`, `__getitem__`, `__setitem__`, `__contains__`), `super()`,
property descriptors, and class-body name resolution rules.

This RFC is co-landed with RFC 0004 (exceptions), because Python's
exception types **are** classes — implementing one without the other
would mean reworking the type machinery twice.

## Motivation

After RFC 0001 the interpreter ran procedural Python: arithmetic,
control flow, functions, closures, comprehensions. That's enough to
run small scripts, but it's almost the worst possible starting point
for running *real* Python code: every nontrivial library file in
CPython's standard library starts with a `class` definition on or
near line 1. Without classes we can't run our own test runner, let
alone CPython's `regrtest`.

A working class system also unlocks several follow-ups that share
machinery: `try`/`except` (RFC 0004) needs exception classes,
generators (RFC 0006) need iterator-protocol objects, the eventual
stdlib bootstrap needs `abc.ABCMeta`, and the object-model rework
(RFC 0002) becomes far easier to specify once we know exactly which
type-slot operations the rest of the interpreter actually invokes.

## CPython reference

This RFC tracks **CPython 3.13**. The primary references are:

- `Objects/typeobject.c` — `type` itself, type creation, slot
  inheritance, `type.__call__` dispatch, MRO computation.
- `Objects/object.c` — generic attribute access, descriptor protocol.
- `Lib/types.py` — `MethodType`, `FunctionType`, `MappingProxyType`.
- The "Data model" chapter of the language reference — the canonical
  list of dunders we're committing to support.
- `Python/compile.c` — class-body compilation, `LOAD_BUILD_CLASS`,
  the `__class__` cell that powers `super()`.

The slice intentionally does **not** track:

- CPython's `tp_*` C-level slot layout. Our `Slots` struct serves the
  same role but exposes only the slots WeavePy currently dispatches
  on. The C-API mapping is RFC 0010 territory.
- Adaptive specialisation / inline cache behaviour around attribute
  access. RFC 0007 (bytecode compaction) will revisit the hot paths.

## Detailed design

### Type objects (`Object::Type`)

A new `Object::Type(Rc<TypeObject>)` variant joins the object enum.
`TypeObject` carries everything needed to call into and inherit from
the type:

```rust
pub struct TypeObject {
    pub name: String,
    pub bases: Vec<Rc<TypeObject>>,
    /// C3 linearisation of `self`, starting at `self` and ending at `object`.
    pub mro: Vec<Rc<TypeObject>>,
    /// Methods, class attributes, properties — keyed by `str` name.
    pub dict: Rc<RefCell<DictData>>,
    /// Fast-path operator slots populated for builtin types (and lazily
    /// for user types that override the corresponding dunder).
    pub slots: Slots,
    /// Marks types in the `BaseException` lineage so the exception
    /// machinery can match without walking the MRO every dispatch.
    pub flags: TypeFlags,
}
```

Every value in the interpreter has a type, surfaced through
`Object::class()`. For the inline variants (`None`, `Bool`, `Int`, …)
the lookup hits a small `BUILTIN_TYPES` registry; for `Object::Instance`
the lookup reads the embedded `class` pointer.

`Object::Class()` / `type(obj)` returns the type object.
`isinstance(x, T)` walks `x`'s MRO and checks for `T`.
`issubclass(A, B)` walks `A`'s MRO and checks for `B`.

### Instances (`Object::Instance`)

```rust
pub struct PyInstance {
    pub class: Rc<TypeObject>,
    pub dict: Rc<RefCell<DictData>>,
}
```

Instance attribute lookup follows CPython's order:

1. Walk the MRO for a *data descriptor* with that name (something
   whose type implements `__set__`). If found, invoke it.
2. Check `instance.dict`. If found, return.
3. Walk the MRO for any other attribute (functions become bound
   methods via `__get__`; class attributes are returned as-is).
4. If nothing matches, raise `AttributeError`.

`STORE_ATTR` and `DELETE_ATTR` skip the descriptor dance for the slice
— they write straight into the instance dict. RFC 0010 will revisit
data descriptors with `__set__`.

### MRO via C3 linearisation

Multiple inheritance follows C3 — the algorithm CPython has used since
2.3. Conflicts raise `TypeError` at class-creation time with the same
"inconsistent hierarchy" message CPython produces.

### Class body compilation

A `ClassDef(name, bases, keywords, body, decorator_list)` AST node
compiles to:

```
LOAD_BUILD_CLASS          # push the magical `__build_class__` builtin
<body as a function>      # function whose body is the class block
LOAD_CONST   <name str>
<each base expr>
<each keyword expr (incl. names tuple)>
CALL  2 + len(bases) + 2 * len(keywords)   # via CALL_KW when keywords
STORE_NAME <name>
```

The class-body function:

- Runs in a special `CodeKind::Class` scope where free names resolve
  via `LOAD_CLASSDEREF` (lookup-by-name-first, then dereference cell
  on miss) rather than plain `LOAD_FAST`. This is what makes
  `class C: x = 1; def f(self): return x` lookup `x` in the class
  namespace.
- Always declares an implicit `__class__` cell so methods that call
  `super()` (which is rewritten by the compiler to read `__class__`)
  pick up the type the class machinery binds after creation.
- Returns the synthesised locals dict via `RETURN_VALUE`.

### Method binding and the descriptor protocol

Function objects gain a `__get__` slot that returns a `BoundMethod`
when the owner is non-None. That's what makes `instance.method`
yield a bound method without explicit machinery in `LOAD_ATTR`.

The slice implements three descriptors at the Rust level:

- `function.__get__` → `BoundMethod` (zero-arg `instance` returns the
  function unchanged, matching CPython).
- `classmethod.__get__` → `BoundMethod` whose receiver is the *class*.
- `staticmethod.__get__` → the underlying function unchanged.

`@property` is supported as a built-in class implementing
`__get__` / `__set__` / `__delete__`. It powers
`@property` / `@x.setter` / `@x.deleter` decorator syntax with the
same getter/setter slot semantics CPython uses.

### `super()`

`super()` with no arguments is rewritten by the compiler into
`super(__class__, <first param>)`. The class machinery binds
`__class__` in a cell when the class body finishes. The runtime
`super` type implements `__getattribute__` to walk MRO starting after
the captured class — same as CPython.

### Operator dispatch (the `Slots` table)

A `Slots` struct holds optional function pointers for the
performance-critical dunders so built-in types don't pay a dict
lookup per arithmetic op:

```rust
pub struct Slots {
    pub bool_:   Option<fn(&Object) -> Result<bool, RuntimeError>>,
    pub len:     Option<fn(&Object) -> Result<usize, RuntimeError>>,
    pub iter:    Option<fn(&Object) -> Result<Object, RuntimeError>>,
    pub next:    Option<fn(&Object) -> Result<Object, RuntimeError>>,
    pub hash:    Option<fn(&Object) -> Result<i64, RuntimeError>>,
    pub eq:      Option<fn(&Object, &Object) -> Result<bool, RuntimeError>>,
    pub lt:      Option<fn(&Object, &Object) -> Result<bool, RuntimeError>>,
    pub call:    Option<...>,
    pub getitem: Option<...>,
    pub setitem: Option<...>,
    pub repr:    Option<fn(&Object) -> Result<String, RuntimeError>>,
    pub str_:    Option<fn(&Object) -> Result<String, RuntimeError>>,
}
```

For user-defined types, slots are left empty; the dispatch falls back
to `type.dict[name]` lookup and a normal Python call.

Binary-op dispatch (`BINARY_OP`, `COMPARE_OP`, etc.) follows CPython's
order:

1. Try `type(a).__op__(a, b)`. If it returns `NotImplemented`,
2. Try `type(b).__rop__(b, a)`. If still `NotImplemented`, raise
   `TypeError("unsupported operand type(s) for OP: …")`.

If `b`'s type is a *strict* subclass of `a`'s type, swap step order
(CPython rule), so subclasses can override the reflected op.

### Built-in types

The following gain real `TypeObject` representations, populated into
`BUILTIN_TYPES` at interpreter startup:

`object`, `type`, `NoneType`, `bool`, `int`, `float`, `str`, `bytes`,
`tuple`, `list`, `dict`, `set`, `range`, `slice`, `function`, `method`,
`builtin_function_or_method`, `code`, `cell`, `super`, `property`,
`classmethod`, `staticmethod`, `BaseException` and its standard
subclasses (see RFC 0004).

`type(obj)` now returns the real type object, so
`isinstance(x, int) is True` works, and `int.__name__ == 'int'`.

### Class-body name resolution

A `CodeKind::Class` scope has unusual name-lookup semantics:

- Writes go to the class's local namespace (same dict that ends up
  in `cls.__dict__`).
- Reads inside the class body but outside any nested function lookup
  in the local namespace first, then fall back to enclosing scope
  (skipping immediate non-class scopes — *the* nested-class quirk).
- Methods (functions defined inside the class body) inherit the
  enclosing function's scope, **not** the class's; this is why
  `class C: x = 1; def f(self): return x` raises `NameError`.

The compiler implements this via a new `LOAD_CLASSDEREF` opcode that
tries the local class namespace first and falls back to the deref
behaviour of `LOAD_DEREF`.

## Drawbacks

- **Size of the object-model rework**. Promoting types to first-class
  objects touches `is_truthy`, `repr`, `eq_value`, `cmp`, `make_iter`,
  `len`, `contains`, and the binary/compare dispatch. Roughly half
  the slice's VM code grows new "is this a user-defined type?" arms.
  We accept the diff size in exchange for a uniform dispatch model.
- **Descriptor protocol only goes one layer deep**. We implement
  function/classmethod/staticmethod/property. Data descriptors and
  `__set_name__` (e.g. for `Enum`) come in a follow-up.
- **No metaclass support beyond `type`**. The class machinery looks
  up `metaclass` keyword, but only `type` is honoured. Other
  metaclasses raise `TypeError("metaclass not yet supported")`.

## Alternatives

- **Implement a separate `UserType` enum variant** rather than
  unifying with built-in types. Rejected: every dispatch site would
  need two branches, and `type(obj)` couldn't return a uniform
  object. CPython doesn't make this distinction either.
- **Skip C3 and use depth-first inheritance**. CPython moved away
  from this two decades ago because it breaks diamond inheritance.
- **Implement `__slots__` now**. Skipped — the slice uses an
  unconditional `__dict__`. RFC 0010 will revisit when we tighten
  memory layout.

## Prior art

- CPython 3.13's `typeobject.c` — the model we're matching.
- RustPython's `rustpython-vm/src/types/typeobject.rs` — close to a
  line-by-line port, modulo their use of `Arc` and a different slot
  enum.
- The 1.0 [_C3_ linearisation paper](https://dl.acm.org/doi/10.5555/646171.683524)
  for the MRO algorithm.

## Unresolved questions

- Whether `super()` should fall back to a generated cell when used in
  a free-function context. CPython raises `RuntimeError` in 3.13; we
  do the same.
- Whether `__init_subclass__` runs as part of the class creation
  protocol. Tracked but deferred — most stdlib code that uses it
  also requires `__set_name__`, and we'd rather land both together.

## Future work

- **RFC 0010**: data descriptors, `__set_name__`, `__init_subclass__`,
  `__slots__`, custom metaclasses beyond `type`.
- **RFC 0006**: generators / coroutines naturally reuse the iterator
  protocol introduced here.
- **RFC 0002**: object-model rework will replace the `Rc<enum>` plus
  separate slot table with a tagged-pointer header that points at the
  type object directly.
