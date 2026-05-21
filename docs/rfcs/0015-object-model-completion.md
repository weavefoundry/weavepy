# RFC 0015: Object-model completion

- **Status**: Accepted
- **Authors**: WeavePy authors
- **Created**: 2026-05-20
- **Tracking issue**: TBD

## Summary

Close the gap between "we have classes and inheritance" (post RFC
0003) and "the rest of Python's object model just works." After this
RFC lands:

- The full descriptor protocol is wired up. `property`,
  `staticmethod`, and `classmethod` are real type objects with their
  own `Object` variants; user code can write `__get__` / `__set__` /
  `__delete__` on any class and the VM applies them in the canonical
  data-descriptor / non-data-descriptor order.
- Metaclasses are first-class. `class Foo(Base, metaclass=Meta)`,
  `type("Name", (Base,), {})`, and `super().__new__(...)` from a user
  metaclass all dispatch through the same VM entry point. `type`
  itself is now every built-in type's metaclass.
- `__new__` / `__init__` split is implemented; `object.__new__`
  provides default instance allocation. A class that defines a
  user `__new__` owns instance construction.
- `__slots__` is enforced. A slot-only class forbids `__dict__` and
  rejects writes to undeclared attributes. Sub-classes that
  re-declare `__slots__` stay slot-only; those that don't pick up a
  normal `__dict__` automatically.
- `__init_subclass__`, `__set_name__`, `__class_getitem__`, and
  user-defined `__instancecheck__` / `__subclasscheck__`
  (PEP 487 + PEP 484 hooks) all dispatch from the build-class and
  attribute-lookup paths.
- Function objects gain a `__dict__`. Decorators that stash
  metadata on the callable (`@functools.wraps`, ABC's
  `__isabstractmethod__`) now work.
- Four frozen Python modules are shipped on top of this machinery:
  **`abc`**, **`enum`**, **`dataclasses`**, **`typing`**.
- Two `functools` extras land alongside the modules:
  **`singledispatch`** and **`cached_property`**.
- Several compiler and VM bug-fixes shake out: `__annotations__`
  population for `class:` and module-level annotated assignments,
  free-variable capture inside f-string format specs and
  comprehensions, reads of the *container* in compound assignment
  targets (`a.b[c] = v`), comma-separated subscript notation
  (`Dict[K, V]`), and a `globals()` builtin that returns the
  caller's module dict.

The combination is what the project calls "Option 1" in the
roadmap: drop in `@dataclass`, `class X(Enum)`, or `class Y(ABC)`
and have it work.

## Motivation

After RFC 0014, ordinary scripts started to run. But the moment a
real-world library reached for the *descriptor protocol* — every
non-trivial framework does, transitively — WeavePy fell over:

- `property` was a thin wrapper that read the getter but didn't
  honour `setter` / `deleter` chaining. Anything that used
  `@x.setter` or `@x.deleter` raised `AttributeError`.
- `classmethod` and `staticmethod` were tagged "functions"; the
  VM had no way to distinguish "bind to the class" from "bind to the
  instance" from "don't bind at all". `Counter.tick(cls)` with a
  user-defined `__init_subclass__` would dispatch wrong.
- `__slots__` parsed without error and was completely ignored. A
  framework that relied on slot enforcement (most of `attrs`,
  `dataclasses(slots=True)`, anything CPython itself ships with
  slots) silently got an attribute dict back.
- Metaclasses worked for the trivial case (`metaclass=type` with no
  override) but `class Color(Enum)` blew up: the metaclass's
  `__new__` was called like a function, not a method, and `super()`
  could not find a parent `__new__` to chain into.
- `__init_subclass__` and `__set_name__` were not invoked at all.
  PEP 487 was a paper standard for us, not a runtime contract.

These individually look like bugs; together they meant the four
stdlib modules everyone reaches for first — `dataclasses`, `enum`,
`abc`, `typing` — could not be shipped. And without those, every
non-trivial Python library hits an `ImportError` on the first
non-stdlib import.

Down-tree, this RFC unblocks:

- Full pydantic-style data classes (`@dataclass(frozen=True,
  order=True)`), which we now ship.
- Protocol-typed APIs and structural duck-typing (`Protocol` plus
  `@runtime_checkable`).
- Any framework that derives from `ABCMeta`, including the *next*
  RFC-tier stdlib modules (`collections.abc.Mapping`,
  `numbers.Real`, etc.).
- Pure-Python implementations of `dataclasses`-shaped frameworks
  (attrs lite, msgspec-shaped APIs).

## CPython reference

This RFC tracks **CPython 3.13**:

- **Descriptors** — Data Model §"Implementing Descriptors", PEP 252
  and CPython's `Objects/descrobject.c`. `property.__get__` /
  `__set__` / `__delete__` are documented in §"Properties".
- **`__slots__`** — Data Model §"__slots__" and PEP 412 ("Key-
  Sharing Dictionary"; slots predate it but interact with it).
  Implementation in `Objects/typeobject.c` (search for
  `subtype_setattro`).
- **Metaclasses** — Data Model §"Determining the appropriate
  metaclass" and §"Customizing class creation". The three-argument
  `type()` form is documented in §"type". `metaclass=` keyword is
  PEP 3115.
- **`__new__` / `__init__`** — Data Model §"Customizing instance
  and subclass checks", §"Creating the class object".
- **PEP 487** — `__init_subclass__` and `__set_name__`. Both run
  at class-creation time, after the body executes but before the
  class object is exposed to user code.
- **PEP 484** — `__instancecheck__` and `__subclasscheck__` on the
  metaclass. The dispatch lives in `Objects/abstract.c`'s
  `PyObject_IsInstance`.
- **`__class_getitem__`** — PEP 560 ("Core support for typing
  module and generic types"). Class-level `__class_getitem__`
  fires when the class itself is subscripted (`MyClass[int]`).
- **`abc`** — `Lib/abc.py` and `Lib/_py_abc.py`. The frozen module
  we ship is a substantially simplified rewrite of the latter; we
  don't carry the C-accelerator fast path.
- **`enum`** — `Lib/enum.py`. We follow the public surface
  (`Enum`, `IntEnum`, `Flag`, `IntFlag`, `auto`, `unique`) without
  the recently-added `StrEnum`, `EnumMeta._missing_` hook, or the
  PEP 663 changes around `__str__` defaults.
- **`dataclasses`** — `Lib/dataclasses.py`. We implement the
  decorator, `field`, `fields`, `asdict`, `astuple`, `replace`,
  `make_dataclass`, `is_dataclass`, and `FrozenInstanceError`.
- **`typing`** — `Lib/typing.py`. We follow the runtime
  introspection surface (`get_origin`, `get_args`, `cast`,
  `runtime_checkable`, `Protocol`, `Generic`, `TypeVar`, `Optional`,
  `Union`, `List` / `Dict` / `Tuple` / etc.).

We deliberately do **not** track:

- PEP 695 type-parameter syntax (`class C[T]:`). The grammar is
  not yet wired up.
- `dataclass(slots=True)`'s slot-redefinition behaviour. We accept
  `slots=True` and produce a `__slots__` attribute but do not
  rebuild the class with the slot descriptors installed.
- `dataclass(kw_only=True)` for individual fields. The class-level
  `kw_only` parameter works; the per-field one does not.
- `typing.ParamSpec`, `typing.TypeVarTuple`, and PEP 695 generic
  syntax (`def f[T](...)`).
- `Protocol`'s callback-protocol form (`class P(Protocol):
  def __call__(...)`). The class itself works, but
  `isinstance(x, P)` does not check the call signature.
- CPython's `__class_getitem__` on built-in types. We only honour
  user-defined `__class_getitem__`.

## Detailed design

### Object model extension

Three new variants are added to `Object`, all of which act as
descriptors when stored on a class:

```rust
pub enum Object {
    // ... existing variants ...
    Property(Rc<PyProperty>),
    StaticMethod(Rc<Object>),     // wraps the underlying callable
    ClassMethod(Rc<Object>),      // wraps the underlying callable
    SlotDescriptor(Rc<SlotDescriptor>),
}

pub struct PyProperty {
    pub fget: Object,   // Object::None when omitted
    pub fset: Object,
    pub fdel: Object,
    pub doc: Object,
}

pub struct SlotDescriptor {
    pub name: String,
    pub class_name: String,
}
```

`PyFunction` gains a `pub attrs: Rc<RefCell<DictData>>` so user
code can assign arbitrary attributes on a function. The cost is one
extra Rc per function; the benefit is `@functools.wraps` and
`abstractmethod` (which sets `__isabstractmethod__` on the wrapped
function) now work without special-casing.

`TypeObject` gains three fields:

```rust
pub struct TypeObject {
    // ... existing fields ...
    pub metaclass: RefCell<Option<Rc<TypeObject>>>,
    pub slot_names: RefCell<Vec<String>>,
    pub forbids_dict: bool,
}
```

`metaclass` is `RefCell<Option<_>>` because `type` is its own
metaclass — we need a write-after-allocation knot. Every built-in
type's metaclass is `type`; this is set up at interpreter startup.

### Attribute lookup: full descriptor protocol

`Vm::load_attr` splits into `load_attr_instance` and
`load_attr_type`, both of which implement the canonical CPython
order:

1. **Data descriptor on the class** (`__get__` plus `__set__` or
   `__delete__`) wins over the instance dict.
2. The **instance dict** wins over a non-data descriptor.
3. **Non-data descriptors** (plain functions, `staticmethod`,
   `classmethod`) bind on access.
4. **`__getattr__` fall-back** if everything else missed.
5. **Synthetic dunders** (`__dict__`, `__class__`) are served from
   the instance / type directly so user code reaching for them
   never sees a fake `AttributeError`.

`Vm::store_attr_instance` mirrors the descriptor path:

1. **User-defined `__setattr__`** on the class overrides everything
   (we only honour Python-level overrides; the implicit
   `object.__setattr__` falls through to the fast path below).
2. **Data descriptor with `__set__`** dispatches to it.
3. **`__slots__` enforcement** — if `forbids_dict` is set, only
   names in `slot_names` may be written.
4. **Default**: write to the instance dict.

`Vm::delete_attr` follows the same pattern.

`property.getter` / `property.setter` / `property.deleter` are
exposed as instance methods on the `property` type so the standard
decorator chain works:

```python
class Temp:
    @property
    def value(self):
        return self._c

    @value.setter
    def value(self, v):
        self._c = v
```

### `__new__` / `__init__` split

`Vm::instantiate` is now a two-phase routine:

1. Walk the MRO looking for the first user-defined `__new__`. If
   none exists, fall back to `object.__new__`, which allocates an
   `Object::Instance(PyInstance::new(cls))`.
2. If `type(instance) is cls`, run `__init__`. Skipping `__init__`
   when `__new__` returned a foreign instance matches CPython.

`object.__new__(cls)` is installed on the `object` type as a
`StaticMethod` (so `cls` is passed positionally and not bound
again). `object.__init__` is a no-op. Both are installed during
`BuiltinTypes::build`.

`type.__new__(mcs, name, bases, ns)` is installed as a sentinel
builtin. When a user metaclass's `__new__` chains through
`super().__new__(...)`, the VM intercepts that call in `Vm::call`
and routes it to `dynamic_type_call_with_meta`, which performs the
real class construction. The sentinel exists so we don't have to
materialise a Python function for `type.__new__`.

### Metaclass dispatch

`type` is now every built-in type's metaclass. After
`BuiltinTypes::build` constructs all the singletons, it iterates
the registry and calls `set_metaclass(type_.clone())` on each. This
gives `isinstance(int, type)` the canonical answer.

`Vm::build_class` resolves the effective metaclass at class-creation
time. The resolution rule mirrors CPython's "determining the
appropriate metaclass":

1. If `metaclass=` is given, use it directly.
2. Otherwise, the most-derived metaclass among the base classes.
3. Fall back to `type`.

Once resolved, the metaclass's `__new__` is invoked; its
`__init__` runs after the class body finishes. If neither is
overridden, we short-circuit to `TypeObject::new_user` for
performance.

When a *type* (`Object::Type`) is called, the VM checks the
metaclass for an overriding `__call__`. `EnumMeta.__call__`,
`ABCMeta.__call__` (we install one to reject abstract
instantiation), and any user metaclass dispatch through this path.
`type` itself does *not* install a `__call__` — it falls through to
`Vm::instantiate` directly to keep the fast path cheap.

Class-level subscripting (`Color["RED"]`, `Container[int]`)
dispatches through:

1. Metaclass `__getitem__` if present (this is how `EnumMeta`
   serves `Color["RED"]`).
2. Class-level `__class_getitem__` otherwise (this is how
   `typing.Generic` serves `Container[int]`).

Iteration on a type (`list(Color)`) checks the metaclass for
`__iter__`. Membership (`Color.RED in container`) checks the
instance class for `__contains__` before falling back to the
built-in container protocol.

### `__slots__`

`Vm::finalize_class_namespace` processes the class body's
`__slots__` after the body runs:

- Each entry becomes a `SlotDescriptor` stored on the class dict
  under its name. The descriptor's `__get__` / `__set__` route to
  the instance dict under the same name (we don't have C-level
  slot storage; the instance dict slot is the closest equivalent
  and still gives us slot-only enforcement).
- `forbids_dict` is set to `true` *only if* every base also
  forbids `__dict__`. The MRO walk treats `object` as forbidding.
  A subclass that doesn't redeclare `__slots__` short-circuits the
  inheritance check and gets a normal `__dict__`.

The enforcement happens in `store_attr_instance`: if `forbids_dict`
is set and the name is not in `slot_names`, we raise
`AttributeError("'X' object has no attribute 'y'")`.

### PEP 487 hooks

`Vm::invoke_set_name_hooks` walks the class namespace after the
body finishes and calls `descriptor.__set_name__(cls, name)` for
every value that defines `__set_name__`. This is how
`@cached_property` learns its attribute name and how user
descriptors register themselves with the owning class.

`Vm::invoke_init_subclass` walks the MRO (skipping the new class
itself) for the first `__init_subclass__`. The hook is treated as
an implicit classmethod regardless of how it was defined — we
unwrap `Object::ClassMethod` if present, then dispatch through a
`BoundMethod` with the new class as receiver. CPython does the same.

`object` carries a no-op `__init_subclass__` (a `ClassMethod`
wrapping a Rust builtin that returns `None`) so any user override
that ends with `super().__init_subclass__(**kwargs)` finds a parent
to chain into.

### `isinstance` / `issubclass` hooks

`b_isinstance` and `b_issubclass` are intercepted in `Vm::call`
when called with the `isinstance` / `issubclass` builtin name. The
intercept dispatches through `do_isinstance_call` /
`do_issubclass_call`, which honour the metaclass's
`__instancecheck__` / `__subclasscheck__` hooks before falling
back to the MRO walk. `ABCMeta`'s `register()` and `Protocol`'s
structural check use this.

### Frozen `abc`, `enum`, `dataclasses`, `typing`

Each module is registered as a `FrozenSource` (see RFC 0014) and
imported on first use.

**`abc`** (~130 lines)

- `ABCMeta` extends `type` and overrides `__init__`,
  `__instancecheck__`, `__subclasscheck__`, and `__call__`.
- `__init__` collects abstract methods declared on the class and
  inherited from bases into a `__abstractmethods__` frozenset.
- `__call__` raises `TypeError` if any abstract method remains;
  otherwise it constructs the instance by invoking `__new__` /
  `__init__` directly. (`super().__call__(...)` doesn't yet work
  because `type.__call__` isn't a real Python method; this is the
  workaround.)
- `register(subclass)` adds the subclass to `_abc_registry`, which
  `__subclasscheck__` walks for virtual-subclass membership.
- `abstractmethod`, `abstractproperty`, `abstractclassmethod`,
  `abstractstaticmethod` set `__isabstractmethod__ = True` on the
  decorated object.
- `ABC` is a convenience base that uses `ABCMeta` as its metaclass.

**`enum`** (~330 lines)

- `EnumMeta` extends `type` and overrides `__new__`, `__call__`,
  `__getitem__`, `__iter__`, `__len__`, `__contains__`,
  `__members__`.
- `__new__` detects member-style class attributes (no leading
  underscore, not callable, not a descriptor), assigns numeric
  values (sequential for `Enum`, power-of-two for `Flag`), and
  builds `_member_map_` / `_value2member_map_`. The instance for
  each member is `object.__new__(cls)` with `_name_` / `_value_`
  set on it.
- Aliases (multiple names mapped to the same value) bind to the
  original member, matching CPython.
- `__call__` implements `Color(1)` value-based lookup; if the
  metaclass is `FlagMeta` and the value isn't in the map, it
  decomposes the integer into a combination of single-bit
  members.
- `FlagMeta` extends `EnumMeta` and adds `_decompose_flag`.
- `Enum` / `IntEnum` / `Flag` / `IntFlag` define `name` /
  `value` properties plus the appropriate dunder methods.
  `IntEnum` and `IntFlag` overload `__add__`, `__sub__`, `__mul__`,
  `__index__`, the comparison dunders, and `__hash__` so they
  interoperate with `int` even though we don't yet support real
  inheritance from `int`.
- `unique` validates that no two members share a value.

**`dataclasses`** (~480 lines)

- `@dataclass` is a class decorator that processes the
  `__annotations__` dict (compiler now populates this — see
  "Compiler changes" below), discovers fields, and generates
  `__init__` / `__repr__` / `__eq__` / ordering methods / `__hash__`
  via Python closures (no `exec`).
- `field(default=..., default_factory=..., init=..., repr=...,
  compare=..., metadata=...)` creates a `Field` sentinel that the
  decorator consumes.
- `MISSING` is a singleton sentinel.
- `_DataclassParams` is a per-class record of the decorator
  arguments (`init`, `repr`, `eq`, `order`, `frozen`).
- `FrozenInstanceError(AttributeError)` is the canonical error
  raised when a frozen dataclass instance is mutated. The
  generated `__setattr__` raises it for any attribute write; the
  generated `__delattr__` does the same for deletes.
- `__init__` for a frozen dataclass uses `object.__setattr__` to
  populate fields, bypassing the user-visible setattr override.
- `fields`, `asdict`, `astuple`, `replace`, `make_dataclass`,
  `is_dataclass` mirror CPython's public surface.

**`typing`** (~390 lines)

- `_SpecialForm` is the marker for non-callable singletons (`Any`,
  `Union`, `Optional`, `Literal`, …). Subscription returns a
  `_GenericAlias`.
- `_GenericAlias` carries `__origin__` and `__args__` plus an
  optional `_name` hint so `List[int]` reads as
  `typing.List[int]` (not `typing.list[int]`).
- `_OriginAlias` wraps `List` / `Dict` / `Tuple` / `Set` /
  `FrozenSet` / `Type` so subscription produces a tagged
  `_GenericAlias`.
- `TypeVar` carries a `__name__`, `bound`, and variance flags;
  it's a runtime marker only (no enforcement).
- `Generic` is a base class; subscribing it (`class C(Generic[T])`)
  works because `_GenericAlias` is iterable in `__class_getitem__`.
- `Protocol(Generic, metaclass=_ProtocolMeta)`. `_ProtocolMeta`
  overrides `__instancecheck__` / `__subclasscheck__` to do the
  structural-typing check when the class is decorated with
  `@runtime_checkable`. The decorator records the candidate
  attributes on the class for later iteration.
- `get_origin`, `get_args`, `cast`, `overload`, `get_type_hints`,
  `runtime_checkable` are all small helpers around
  `_GenericAlias`.

### `functools.singledispatch` and `cached_property`

`singledispatch` is implemented as a class (`_SingleDispatchCallable`)
rather than nested closures. This sidesteps a current compiler
limitation in three-level freevar passthrough — the inner
`@register(cls)` decorator could not see the outer `registry`. The
behaviour matches `functools.singledispatch`: dispatch on the runtime
type of the first argument, walking the MRO; alternative
implementations are registered via `gf.register(type)`.

`cached_property` is a small data descriptor:

- `__set_name__(owner, name)` records the attribute name.
- `__get__(instance, owner)` checks `instance.__dict__` for a
  cached value (using `_MISSING` as the sentinel because `None` is
  a valid cached value), and writes the computed value back into
  `instance.__dict__` so subsequent accesses bypass the descriptor.

### Compiler changes

- **`AnnAssign`** (annotated assignment) now populates
  `__annotations__` in class and module bodies. The compiler
  tracks an `annotations_initialized` flag and emits a one-time
  `__annotations__ = {}` followed by a per-field
  `__annotations__["x"] = int` for each annotation. Without this,
  `@dataclass` would receive no fields to process.
- **`code_kind`** (`Module` / `Function` / `Class`) lets
  `AnnAssign` decide whether to populate `__annotations__`
  (Module / Class) or treat the annotation as a no-op (Function
  body — Python discards function-local annotations).
- **Free-variable capture in f-strings** (`f"{self.name}"`) and
  in comprehensions (`[f.name for f in fields(cls)]`) now walks
  `ExprKind::FormattedValue` and `ExprKind::JoinedStr` properly.
  Previously the compiler missed `self` as a free variable when it
  appeared inside an f-string nested in a list comprehension.
- **Compound assignment target reads** (`a.b[c] = v`) are now
  collected as reads of `a` and `c`. Without this, nested
  functions couldn't see outer-scope variables that were only
  used as the *container* of an assignment target.
- **Comma-separated subscript** (`Dict[K, V]`, `Tuple[A, B, C]`)
  is parsed as a tuple, matching CPython's slice protocol.
- **Trailing comma in `from x import (a, b,)`** is now tolerated.

### VM changes (other than the above)

- `globals()` returns the active frame's globals (the calling
  function's defining module). Without this, frozen helpers
  couldn't peek at sibling names — `enum.py` for example needs to
  see `FlagMeta` to decide whether to use power-of-two values.
- `Object::eq_value` now falls back to reference identity for
  `Type` / `Module` / `Function` / `Builtin` / `Instance`. CPython
  does the same — `type(None) in (int, type(None))` should be
  `True` because there's exactly one `NoneType` object.
- `hash()` builtin is added. It intercepts in `Vm::call` and
  dispatches through the instance's `__hash__` when defined,
  raising `TypeError: unhashable type` for `__hash__ = None`.
- `dir()` builtin is added. Walks the class MRO, instance dict,
  and module dict; returns a sorted list of names. Used by the
  Protocol structural check and by anything reaching for
  introspection.
- `str.join(gen_expr)` now drains the generator via VM-aware
  iteration before joining. The static `str_join` builtin can't
  drive a Python generator on its own.
- `getattr` / `setattr` / `hasattr` now handle `Object::Function`
  (read/write into `f.attrs`).
- `getattr(typeobj, "__name__")` now returns the synthetic name
  even when no entry exists in the type dict, matching the direct
  attribute access path.
- `BaseException.__init__(self, *args)` is installed to set
  `self.args` automatically. Without this, user-defined exception
  subclasses had no `args` tuple.
- `object.__setattr__` and `object.__delattr__` are installed as
  builtins so `dataclasses`-style frozen `__init__` can call
  `object.__setattr__(self, name, value)` to bypass the frozen
  override.

## Drawbacks

- **No real inheritance from `int`.** `IntEnum` and `IntFlag`
  overload arithmetic and comparison dunders explicitly because we
  can't yet make user classes inherit from `Object::Int`. The
  surface matches CPython but `isinstance(Priority.LOW, int)` is
  `False` here and `True` upstream. Tracked in "Future work".
- **`type.__call__` is not a real Python method.** `ABCMeta.__call__`
  has to invoke `__new__` / `__init__` directly rather than calling
  `super().__call__(...)`. The behaviour is identical; the code
  reads slightly differently. Lifting this requires plumbing
  `type` itself as a fully Python-visible callable.
- **`Protocol` doesn't check callability of `__call__` protocols.**
  The structural check looks at attribute *presence*, not call
  signature. A real `runtime_checkable` would require introspecting
  `Signature`, which we haven't implemented.
- **`dataclass(slots=True)` accepts the argument but doesn't rebuild
  the class.** The current implementation sets `__slots__ = (...)`
  on the class dict; CPython actually constructs a new class with
  the slot descriptors installed. The behavioural difference is
  small in practice (the slot enforcement still kicks in for any
  attribute access) but observable for code that compares the
  class object itself.
- **`__annotations__` population is a class-body / module-body
  side effect.** Function-local annotations (`def f(x: int) -> int:
  y: str = "a"`) are still compiled away. CPython preserves them
  too — they're only emitted into the function's globals if the
  module is loaded with `from __future__ import annotations`. We
  match that behaviour.
- **PEP 695 syntax (`class C[T]:`) is not supported.** This is the
  modern way to declare generics; our `typing` module ships the
  `TypeVar` / `Generic[T]` form instead. Adding PEP 695 is a
  parser-level change tracked separately.
- **No real `Generic[T]` parameter check.** `Container[int](5)`
  works syntactically but the `int` is discarded. CPython also
  discards it at runtime; the difference is that mypy / pyright
  *would* enforce it statically, which we don't.

## Alternatives

- **Implement descriptors as Rust trait objects.** Would let us
  inline the common cases (`property` getter) without a Python
  function call. Rejected for the slice — the indirection is
  cheap compared to user-defined `__get__` and we'd lose the
  ability to introspect descriptors as plain Python objects.
- **Vendor CPython's actual `Lib/dataclasses.py`.** Rejected: it
  uses `exec()` to build the generated methods, which we don't
  support, plus uses several private CPython internals. The
  closure-based rewrite is smaller and easier to maintain.
- **Skip `typing` and require users to write `from __future__
  import annotations`.** Rejected: the four-module set sells
  itself; shipping three of them without the fourth would be
  oddly asymmetric.
- **Implement `__slots__` with C-level slot storage.** Rejected
  for now — we'd need to make `PyInstance` polymorphic over slot
  vs. dict storage, doubling the variant count. The "slot
  descriptors write to the dict but enforce names" approach is
  observable from Python only in micro-benchmarks.

## Prior art

- **CPython 3.13** is the conformance target. Where we diverge,
  the divergence is documented and intentional.
- **RustPython** ships a similar slice of object-model machinery
  (descriptors, metaclasses, slots) and similarly faces the
  "inherit from int" wall for `IntEnum`. They solve it by
  implementing the C API for built-in types, which we have not yet
  committed to.
- **MicroPython** ships a much smaller object model (no
  `Protocol`, no `dataclass`, no `metaclass=`). They don't
  pretend to ship the full Python object model.
- **GraalPy** runs a vendored CPython object model via Truffle.
  Out of scope for our slice but the most faithful reference for
  edge-case behaviour.

## Unresolved questions

- **Reference-identity hash for user instances.** A user instance
  is currently hashed by *structural* equality (always equal to
  itself if the underlying primitive bytes match). For real Python
  semantics, `set` and `dict` membership should use the user
  `__hash__` / `__eq__` pair. Today this means two distinct frozen
  dataclass instances with the same fields hash to different
  buckets and the set "deduplicates" wrong. Tracked as a follow-up.
- **`type.__call__` as a Python method.** Several user-metaclass
  patterns (`super().__call__(...)`) would benefit. The blocker is
  exposing the C-level `type_call` as a Python-visible method
  without breaking the fast-path optimisation.
- **`__init_subclass__` kwargs forwarding.** We currently
  forward `subclass_kwargs` verbatim. CPython filters out the
  `metaclass` keyword before forwarding. Easy to add when needed.
- **Inheriting from `int` for `IntEnum`.** The blocker is more
  fundamental: `Object::Int` is a Rust enum variant, not a class.
  A user `IntEnum` member that needs to behave as an `int` would
  need either a wrapper type or a real "subclass an immutable
  primitive" story.

## Future work

- **Real inheritance from `int`, `str`, `tuple`, `list`, `dict`.**
  Would let `IntEnum(int, Enum)` work without dunder overloading,
  and would let frozen `collections.OrderedDict` inherit from
  `dict` properly. Largest remaining object-model gap.
- **PEP 695 type-parameter syntax** in the parser, plus the
  associated `__type_params__` plumbing.
- **`__class_getitem__` on built-in types** so `list[int]`,
  `dict[str, int]`, etc. work without `from typing import List`.
- **`typing.NamedTuple`** — currently we ship `namedtuple` from
  `collections`; the typing-flavoured variant has slightly different
  metaclass semantics that we haven't tracked.
- **Custom descriptor classes with `__set_name__` chains** —
  works today, but the error reporting when `__set_name__` raises
  is generic; CPython's is field-aware.
- **`functools.total_ordering`** — fills in the comparison
  dunders from `__eq__` + one of `<`/`<=`/`>`/`>=`. Easy follow-
  up now that the object model supports it.
- **`Protocol.__call__` shape check** — would require Signature
  introspection.
- **`abc.ABCMeta._abc_registry` invalidation** — we use a plain
  set; CPython invalidates a weakref cache when subclasses are
  added. We don't have weakrefs; this is non-blocking for now.
