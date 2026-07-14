# RFC 0051: Conformance wave 6 — verbatim `typing`, PEP 695 runtime, and the core-language burn-down

- **Status**: Implemented
- **Authors**: WeavePy authors
- **Created**: 2026-07-12
- **Tracking issue**: TBD
- **Builds on**: RFC 0049 (wave 5 — measured whole-suite baseline),
  RFC 0050 (Unicode/codecs + the `linejump.rs` legal-jump analysis),
  RFC 0033 (CPython-faithful code objects), RFC 0037/0038/0048
  (earlier conformance waves and the verbatim-stdlib adoption policy).

## Summary

Wave 5 left a measured whole-suite baseline: 226 of 427 vendored
CPython 3.13 `Lib/test/` labels pass, with every red row carrying a
measured first-failure. Reading those rows, the remaining reds are not
random: the single largest cluster is the **core language and object
model** (`test_builtin`, `test_types`, `test_call`, `test_super`,
`test_descrtut`, `test_dictviews`, `test_scope`, `test_metaclass`,
`test_slice`, `test_index`, …), and the single most load-bearing
non-verbatim stdlib module left is **`typing.py`** — a ~1,570-line
shim standing in for CPython's ~3,834-line module that nearly every
modern package imports at module level.

Wave 6 replaces the shim with **CPython 3.13's verbatim `typing.py`**
over a new CPython-faithful **`_typing` support module** (`TypeVar`,
`ParamSpec`, `TypeVarTuple`, `ParamSpecArgs`, `ParamSpecKwargs`,
`TypeAliasType`, `Generic`, `NoDefault`, `_idfunc`), upgrades the
**PEP 695 front-end** from name-only desugaring to full syntax capture
(bounds, constraints, PEP 696 defaults, `*Ts`, `**P`) with lazy
evaluation, and burns down the measured **core-language cluster** that
both `typing` and everything after it depends on. The wave folds in
two cheap, high-value tooling items whose groundwork already exists:
**writable `frame.f_lineno`** (finishing RFC 0050's `linejump.rs` on
top of the trace hooks from RFC 0031) and the legacy **`co_lnotab`**
code-object attribute.

As with every wave since RFC 0036, the deliverable is *measured*: the
full sweep is re-run, `tests/regrtest/expectations.toml` is rewritten
from evidence, and every remaining red carries an actionable
first-failure reason.

## Motivation

1. **`typing` is the drop-in gate.** pydantic, FastAPI, attrs, SQLAlchemy,
   dataclasses-heavy code — practically all of modern PyPI — execute
   `import typing` (and increasingly `from typing import ...` of 3.12+
   names) at import time. The current shim omits `NoDefault`,
   `ParamSpec.kwargs`, `Protocol` edge semantics, `get_protocol_members`,
   `TypeAliasType` fidelity, and dozens of smaller surfaces. Each gap is
   an import-time `ImportError`/`AttributeError` in real packages, not a
   subtle behavioral drift. Five measured labels are red on it today:
   `test_typing`, `test_type_params`, `test_type_aliases`,
   `test_genericalias`, `test_genericclass`.

2. **The core-language cluster blocks everything downstream.** Verbatim
   `typing.py` is itself a brutal object-model acceptance test: it needs
   faithful descriptors, `__mro_entries__`, `__class_getitem__`,
   `__init_subclass__` kwargs, slots on heap types, method resolution on
   `super`, and `types.GenericAlias` parity. The same gaps are the
   measured first-failures of `test_builtin`, `test_call`, `test_super`,
   `test_types`, `test_dictviews`, `test_descrtut` et al. Fixing them
   once flips whole groups of labels and de-risks waves 7+ (tooling,
   deployment surface, asyncio).

3. **The tooling items are half-built.** RFC 0050 shipped
   `linejump.rs` — the CPython 3.13 legal-jump analysis for
   `frame.f_lineno` assignment — but the setter itself never landed;
   it is the dominant residual in `test_sys_settrace` (159 failures).
   `co_lnotab` is a small backwards-compatibility property over the
   already-shipped PEP 626 `co_linetable`, and is the measured
   first-failure of `test_dis`.

4. **Cost of inaction.** The README's drop-in claim is graded by the
   sweep. Leaving `typing` as a shim caps the conformance number and
   invalidates any "run your real project" story the moment a package
   imports a missing name.

## CPython reference

- `Lib/typing.py` at v3.13 (vendored checkout: `vendor/cpython/Lib/typing.py`,
  3,834 lines) — adopted verbatim.
- `Objects/typevarobject.c` — the C implementations of `TypeVar`,
  `ParamSpec`, `TypeVarTuple`, `ParamSpecArgs`, `ParamSpecKwargs`,
  `TypeAliasType`, `Generic`, and the `NoDefault` singleton that
  `Modules/_typingmodule.c` re-exports as `_typing`. WeavePy re-implements
  this surface in Python (frozen `_typing`), matching observable behavior.
- PEP 695 (type parameter syntax), PEP 696 (type parameter defaults,
  new in 3.13), PEP 646 (`TypeVarTuple`), PEP 612 (`ParamSpec`),
  PEP 705 (`ReadOnly`), PEP 742 (`TypeIs`).
- `Python/intrinsics.c` — `INTRINSIC_TYPEVAR{,_WITH_BOUND,_WITH_CONSTRAINTS}`,
  `INTRINSIC_PARAMSPEC`, `INTRINSIC_TYPEVARTUPLE`, `INTRINSIC_TYPEALIAS`,
  `INTRINSIC_SET_FUNCTION_TYPE_PARAMS` — the shape WeavePy's lowering
  mirrors (as named `__weavepy_*__` VM intrinsics rather than new opcodes).
- `Objects/frameobject.c` — `frame_setlineno` (the `f_lineno` setter
  semantics: legal-jump computation, exact error messages, block-stack
  adjustment) and `lnotab_notes.txt` / `Objects/codeobject.c` for the
  deprecated `co_lnotab` encoding derived from `co_linetable`.
- Acceptance tests: `Lib/test/test_typing.py`, `test_type_params.py`,
  `test_type_aliases.py`, `test_genericalias.py`, `test_genericclass.py`,
  `test_builtin.py`, `test_call.py`, `test_super.py`, `test_types.py`,
  `test_dictviews.py`, `test_descrtut.py`, `test_slice.py`,
  `test_index.py`, `test_enumerate.py`, `test_scope.py`,
  `test_sys_settrace.py`, `test_dis.py`, and the rest of the measured
  core-language rows in `tests/regrtest/expectations.toml`.

## Detailed design

### WS1 — `_typing` support module + verbatim `typing.py`

**`_typing` (new frozen module, pure Python).** CPython implements the
type-parameter objects in C purely for speed; their semantics are fully
observable from Python and thoroughly tested. WeavePy ships a frozen
`_typing.py` implementing, faithfully:

- `TypeVar` — `__name__`, `__bound__` / `__constraints__` (computed
  lazily through `evaluate_bound` / `evaluate_constraints` thunks for
  PEP 695 syntax; eager for the classic constructor), `__default__` /
  `has_default()` (PEP 696, `NoDefault` sentinel), `__covariant__`,
  `__contravariant__`, `__infer_variance__`, `__typing_subst__`,
  `__typing_prepare_subst__`, `__reduce__`, `__or__`/`__ror__`,
  module-name capture, and the exact `TypeError` messages for invalid
  variance combinations.
- `ParamSpec` (+ `ParamSpecArgs`/`ParamSpecKwargs` with `__origin__`,
  equality, reprs `P.args`/`P.kwargs`), `__typing_subst__` /
  `__typing_prepare_subst__` matching `typevarobject.c`.
- `TypeVarTuple` — `__typing_subst__` raising (substitution handled by
  the prepare hook), `__typing_prepare_subst__` implementing the PEP 646
  splat algorithm, `has_default()`.
- `TypeAliasType` — lazy `__value__` (evaluated once from the compiler
  thunk, then cached), `__type_params__`, `__parameters__`,
  `__module__`, `__name__`, subscription producing a `_GenericAlias`,
  `__or__`, refusal to be subclassed/instantiated-oddly, exact reprs.
- `Generic` — a real class whose `__class_getitem__` and
  `__init_subclass__` delegate to `typing._generic_class_getitem` /
  `typing._generic_init_subclass` via lazy import, exactly like the C
  version (this is what lets `typing.py` say
  `from _typing import Generic` and still keep the machinery in Python).
- `NoDefault` — singleton with `repr` `typing.NoDefault`, unpicklable
  shape matching C, and `_idfunc`.

**Verbatim `typing.py`.** `crates/weavepy-vm/src/stdlib/python/typing.py`
is replaced by the vendored CPython 3.13 file, byte-for-byte, per the
adoption policy from RFC 0048 (verbatim stdlib wherever a support
surface can carry it). Divergences, if forced, are documented inline
with `# WEAVEPY:` markers — target is zero.

**Ripples.** Verbatim `typing` imports `_collections_abc`, `abc`,
`contextlib`, `copyreg`, `functools`, `operator`, `sys`, `types` — all
already frozen. It also leans on object-model behaviors enumerated in
WS3 (they are the *point* of this wave, not incidental).

### WS2 — PEP 695/696 front-end capture and lazy lowering

**Parser** (`weavepy-parser`): `Vec<String>` type params become a real
AST node:

```rust
enum TypeParamKind { TypeVar { bound: Option<Box<Expr>> },
                     TypeVarTuple, ParamSpec }
struct TypeParam { name: String, kind: TypeParamKind,
                   default: Option<Box<Expr>>, span: Span }
```

`collect_pep695_type_params` stops discarding `*`/`**`, bounds, and
`=` defaults, and rejects the combinations CPython rejects
(e.g. bounds on `TypeVarTuple`/`ParamSpec`) with CPython's messages.

**Compiler** (`weavepy-compiler`): the prologue lowers each parameter
to the matching VM intrinsic —
`__weavepy_typevar__(name)`,
`__weavepy_typevar_with_bound__(name, <lazy bound thunk>)`,
`__weavepy_typevar_with_constraints__(name, <lazy tuple thunk>)`,
`__weavepy_paramspec__(name)`, `__weavepy_typevartuple__(name)` —
each with an optional trailing `default` thunk (PEP 696). Bounds and
defaults compile as zero-arg lambdas so evaluation is deferred to
first access, matching PEP 695 lazy semantics. `type X[T] = V`
routes the same objects into the existing `__weavepy_type_alias__`
intrinsic, which now constructs `_typing.TypeAliasType` instead of the
shim class. The epilogue continues to stamp `__type_params__` and drop
the temporary bindings.

*Known approximation:* CPython evaluates type params and lazy values
inside a dedicated hidden scope (`<generic parameters of f>`) with its
own qualname rules. WeavePy keeps the same-scope bind-then-del
desugaring from RFC 0033-era lowering; the divergence is observable
only via frame introspection inside bound/default thunks and is
recorded in the expectations rows it affects.

**VM** (`weavepy-vm`): the intrinsics import the frozen `_typing` on
first use (cached), construct the objects with `_lazy_eval` thunks
attached, and are invisible to user code (`dir()`-hidden builtins, as
today).

### WS3 — Core-language burn-down

Work the measured first-failures of the core cluster, in dependency
order. The list below names the *measured* blockers from
`expectations.toml`; each fix must reproduce first via the vendored
test, then land with a bundled regrtest where the surface is novel:

- **Calls/functions**: `enumerate()` and other C-shaped constructors
  accepting documented keyword arguments (`iterable`/`start`);
  argument-clinic arity errors for builtins that reject kwargs
  (`test_call`'s `builtin '__init__' does not accept keyword
  arguments`); method objects delegating attribute access to
  `__func__` (`test_funcattrs`).
- **Descriptors/type system**: `super().mro`/attribute lookup through
  the type (`test_super`); slot-descriptor `__get__` binding errors
  (`test_abstract_numbers`); `DynamicClassAttribute.__isabstractmethod__`
  (`test_dynamicclassattribute`); `type()` two-arg/three-arg error
  shapes and metaclass doctests (`test_metaclass`, `test_types`);
  `types.GenericAlias` semantics — `fromkeys`, `__or__`, hashing,
  proxying rules (`test_genericalias`, `test_defaultdict`,
  `test_genericclass`).
- **Builtins/objects**: hashable `slice` (3.12+ semantics,
  `test_slice`); dict-view set-op operand strictness (`test_dictviews`);
  `__index__`-driven sequence repetition (`test_index`);
  `float.hex` normalization (`'0x1.0p+0'`, `test_pow`/`test_float`
  residuals); `int` `__float__`-less coercion path (`test_cmath`);
  `str.maketrans` argument validation (`test_str`);
  `setattr` non-string attribute `TypeError` (`test_baseexception`);
  `ExceptionGroup` refusing `BaseException` members with CPython's
  message (`test_exception_group`); module `__annotations__` lazy
  creation (`test_module`); `eval()` globals-type `TypeError`
  (`test_dynamic`).
- **Scoping/compile**: the four residual `test_scope` failures
  (class-body free variables), `test_global`'s `NameError` timing,
  comprehension scoping doctests (`test_genexps`, `test_dictcomps`,
  `test_listcomps`, `test_setcomps` where measured red), and
  `SyntaxError` message parity where the sweep names it
  (`test_syntax`'s `'invalid syntax'` substring, `test_future_stmt`).
- Whatever the re-measured sweep surfaces as flipping within budget —
  the wave-5 rule stands: fixes are capacity-bounded, the sweep is not.

### WS4 — Writable `frame.f_lineno` + `co_lnotab`

- `frame.f_lineno` gains a setter, valid only from a `'line'` trace
  event (CPython rule), computing the legal-target set via the
  existing `linejump.rs` analysis and raising CPython's exact
  `ValueError` texts for illegal jumps (into/out of `try`/`with`/
  `except` blocks, into exception handlers, onto lines that don't
  exist). A legal jump rewrites the frame's instruction pointer and
  unwinds/adjusts the block stack the way `frame_setlineno` does.
- `code.co_lnotab` — the deprecated pre-PEP-626 encoding — is derived
  on demand from the `cpython_code` codec's line table, matching
  CPython's `co_lnotab` byte output (including the signed-delta
  wraparound rules), so `dis` internals and legacy tools that still
  read it keep working.
- Targets: `test_sys_settrace` (159 measured residual failures are
  jump tests), `test_dis` first-failure, `pdb`'s `jump` command.

### WS5 — Re-measure and re-baseline

Per the RFC 0049 protocol: two full sweeps
(`weavepy-conformance regrtest --all-cpython --mode subprocess`),
cross-checked; `expectations.toml` rewritten so every row is measured;
newly-green rows get documenting `pass` entries where non-obvious;
bundled fixtures stay green; new bundled regrtests land for the novel
surfaces (`_typing` object semantics, PEP 695 lazy evaluation,
`f_lineno` jumps, `co_lnotab` round-trip).

### Acceptance criteria

1. `import typing` executes CPython's verbatim 3.13 `typing.py`.
2. `cpython/Lib/test/test_type_params.py`, `test_type_aliases.py`, and
   `test_genericclass.py` flip to measured `pass`;
   `test_typing.py` and `test_genericalias.py` flip to `pass` or, at
   minimum, to measured rows whose residuals are enumerated and small
   (they are the two largest suites in the cluster).
3. At least 15 net labels flip red→green on the full sweep versus the
   wave-5 baseline, concentrated in the core-language cluster.
4. `frame.f_lineno` assignment works for the CPython legal-jump set;
   `test_sys_settrace`'s residual count drops accordingly (measured).
5. `cargo fmt` / `clippy -D warnings` / `cargo test --workspace` /
   `regrtest --check` all green.

## Drawbacks

- **Pure-Python `_typing` is slower than C.** Every generic class
  creation walks Python-level `__init_subclass__`. Accepted: waves
  are correctness-first; a Rust-native `_typing` fast path is future
  performance work, and the module boundary makes the swap invisible.
- **Verbatim `typing.py` is unforgiving.** It will surface object-model
  bugs beyond the enumerated list; the wave's WS3 budget absorbs what
  it can and the sweep records the rest. This is by design (same
  policy that made RFC 0048's verbatim `test.support` adoption pay off).
- **The PEP 695 hidden-scope approximation** leaves a known observable
  divergence (frame introspection inside lazy thunks). Recorded, bounded,
  and fixable later without changing the `_typing` surface.

## Alternatives

- **Extend the shim incrementally** (add `NoDefault`, `ParamSpec.kwargs`,
  …): rejected. The shim's divergence surface is unbounded — every new
  PyPI release finds a new missing name — and the maintenance cost
  already exceeds the one-time cost of the support module. Verbatim
  adoption is the policy every previous wave validated (`re`,
  `collections`, `datetime`, `test.support`, `email`, `http`, codecs).
- **Implement `_typing` in Rust from day one**: rejected for this wave.
  The semantics live in `typevarobject.c`'s ~2,000 lines of subtle
  substitution logic; Python-first gets correctness cheaply and the
  tests then guard a later Rust port.
- **New dedicated opcodes for PEP 695 intrinsics** (CPython's
  `CALL_INTRINSIC_1/2`): unnecessary — WeavePy's named-intrinsic calls
  compile to ordinary `Call` opcodes and the `cpython_code` codec
  already handles novel lowerings; adding opcodes would ripple through
  the JIT and the codec for zero conformance gain.

## Prior art

- **CPython 3.12** made exactly this split (`typing.py` +
  C `_typing`/`typevarobject.c`) when PEP 695 landed; the delegation
  trick for `Generic.__class_getitem__` is theirs.
- **typing_extensions** maintains pure-Python re-implementations of the
  same objects for older Pythons — evidence the C surface is fully
  expressible in Python, and a behavioral cross-reference during
  implementation.
- **PyPy** runs CPython's `typing.py` verbatim over a pure-Python
  `_typing` equivalent; no compatibility issues attributable to the
  pure-Python substitution are on record.

## Unresolved questions

- Whether `test_typing.py` can go fully green inside the wave budget
  (10K+ lines, deep `Protocol`/`get_type_hints` corners) or lands as a
  small measured residual. Acceptance criterion 2 allows either.
  *(Resolved: fully green — see implementation results.)*
- Whether the PEP 695 hidden scope needs real compiler support before
  wave 7's `inspect`/tooling work (frame-walking debuggers may observe
  the approximation). *(Resolved: the real hidden scope landed in this
  wave — see implementation results.)*

## Future work

- Rust-native `_typing` fast path once behavior is locked by tests.
- Wave 7: developer-tooling parity (`_lsprof`, closure-cell
  introspection, `doctest`/`unittest` residuals) — several of its rows
  shrink as WS3 lands.
- Wave 8: deployment surface (import machinery, `zipimport` speed,
  `venv`/`site`/CLI parity).

## Implementation results (measured)

All five acceptance criteria are met. The re-baselined sweep
(two cross-checked full runs, `--mode subprocess --jobs 8`, then a
`--check`-graded verification run) grades **254 pass / 153 fail /
13 skip / 7 timeout over the 427 vendored-CPython labels** — up from
226 pass at the wave-5 baseline (**+28 net**, against the ≥15 the RFC
asked for) — plus all 80 bundled fixtures green.

**WS1 — verbatim `typing`.** `crates/weavepy-vm/src/stdlib/python/typing.py`
is byte-for-byte identical to `vendor/cpython/Lib/typing.py` (3,834
lines, zero `# WEAVEPY:` divergence markers — the target was met), over
a new 971-line frozen `_typing` implementing `TypeVar`, `ParamSpec`
(+`args`/`kwargs`), `TypeVarTuple`, `TypeAliasType`, `Generic`,
`NoDefault`, and `_idfunc` with the C module's observable semantics.
`test_typing` — the RFC's "can it go fully green?" unresolved question —
resolved **fully green** (686 run, 2 skipped), as did `test_typing`'s
whole cluster: `test_type_params` (104 run), `test_type_aliases`,
`test_genericalias`, `test_genericclass`.

**WS2 — PEP 695/696 front-end.** The parser's `TypeParam`/`TypeParamKind`
AST captures bounds, constraints, PEP 696 defaults, `*Ts`, and `**P`
with CPython's rejection messages; the compiler lowers them to the
`__weavepy_typevar__`-family intrinsics with lazy thunks. The "known
approximation" (same-scope bind-then-del) turned out not to be needed:
a real hidden **`<generic parameters of X>` annotation scope** landed
in the compiler, with CPython's qualname rules — which is what let the
`test_type_params` frame-introspection cases pass rather than being
recorded as divergences.

**WS3 — core-language burn-down.** Thirteen labels flipped measured
red→green with zero regressions: `test_typing`, `test_type_params`,
`test_type_aliases`, `test_genericalias`, `test_genericclass`,
`test_super`, `test_scope`, `test_slice`, `test_index`,
`test_dictviews`, `test_descrtut`, `test_dynamic`, `test_enumerate`.
A late `types.GenericAlias` fix (found by the sweep via
`test_dataclasses`): subclass construction now allocates through `cls`
like CPython's `ga_new`, so `type(instance)` reports the subclass and
`@dataclass class A(types.GenericAlias)` round-trips through
`is_dataclass`. Verbatim `graphlib`, `filecmp`, and `mailbox` were
adopted along the way, and a CPython-`ast_unparse.c`-mirroring
expression unparser (`weavepy-parser/src/unparse.rs`) landed for
PEP 563 annotation text.

**WS4 — `f_lineno` + `co_lnotab`.** `frame.f_lineno` is writable from
`'line'` trace events over the RFC 0050 `linejump.rs` analysis, with
CPython's exact `ValueError` texts. `test_sys_settrace`'s residual
dropped from **159 failures + 16 errors to 58 failures + 0 errors**
(449 run), with only 6 jump-matrix cases left (comprehension-inlining
`RuntimeWarning` divergences among them). `co_lnotab` ships;
`test_dis`'s first failure moved past it to the opcode-coverage arc.

**WS5 — re-baseline.** `tests/regrtest/expectations.toml` was rewritten
from the two cross-checked sweeps: 13 rows flipped to documented `pass`,
no row regressed, and the handful of correct-but-slow labels that flip
verdicts under `-j8` contention (`test_range`, `test_unittest`,
`test_re`, `test_random`, `test_codecencodings_iso2022`) gained
per-test `timeout_seconds` headroom so the recorded verdict is the
load-independent one. One harness bug was fixed en route: the
subprocess runner's detail truncation sliced at byte 1024 mid-UTF-8
sequence and panicked the worker (`truncate_detail` now backs off to a
char boundary).

Gates: `cargo fmt --check`, `cargo clippy --workspace --all-targets
-- -D warnings`, and `cargo test --workspace` are green, and the final
sweep grades clean against the rewritten baseline.
