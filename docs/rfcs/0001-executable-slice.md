# RFC 0001: The executable slice

- **Status**: Accepted
- **Authors**: WeavePy authors
- **Created**: 2026-05-19
- **Tracking issue**: TBD

## Summary

Replace the pre-alpha pipeline stubs with a real, end-to-end implementation
of a meaningful subset of Python 3.13. After this RFC lands, WeavePy can
execute non-trivial programs: arithmetic, control flow, functions,
closures, comprehensions, lambdas, and the common builtins. This is the
first commit where WeavePy *does* anything; everything before it has been
scaffolding and tooling.

## Motivation

The repository's first commit scaffolded six crates wired end-to-end, but
every phase is a no-op: the lexer produces a single EOF token, the parser
returns an empty module, the compiler emits an empty code object, and the
VM unconditionally returns `None`. The conformance harness exists and
correctly reports 0% match against CPython on tokens (with AST and dis
skipped because there's no WeavePy output to compare).

We can't validate any architectural decision until the pipeline actually
runs code. Building a single layer to completion before moving down is a
known failure mode for greenfield interpreters because there's no
end-to-end feedback. The "executable slice" inverts that: implement enough
of every phase to make a coherent subset of Python work, and let real
inputs surface design problems while they're still cheap to fix.

A working slice also unlocks the conformance harness: the `tokens` row
should move from 0% to ~90% on the in-tree corpus, and the `ast` / `dis`
rows graduate from `Skipped` to live diffs against CPython 3.13.

## CPython reference

The slice tracks CPython 3.13. Specifically:

- **Lexer**: `Lib/tokenize.py`, `Parser/tokenizer.c`, `Lib/token.py`. Token
  names emitted by the conformance normalizer come from `tokenize.tok_name`.
- **Parser / AST**: `Parser/Python.gram` (the PEG grammar), `Lib/ast.py`,
  the public `ast` module documentation. AST node names and field names
  match `ast.dump(...)` output.
- **Compiler / bytecode**: `Python/compile.c`, `Lib/dis.py`,
  `Include/internal/pycore_opcode.h`. Opcode names match CPython 3.13's,
  notably `BINARY_OP` (with the 3.11+ NB\_ADD/SUB/... sub-op tag),
  `COMPARE_OP`, `CALL`/`RETURN_VALUE`/`RESUME`, `LOAD_FAST`/`STORE_FAST`,
  `LOAD_DEREF`/`STORE_DEREF`/`MAKE_CELL`, `GET_ITER`/`FOR_ITER`/`END_FOR`,
  `MAKE_FUNCTION`, etc.
- **Runtime semantics**: CPython's data model documentation (especially
  the `object.__bool__` / `__len__` / `__eq__` / `__hash__` slots) and
  the language reference's "Expressions" chapter for operator precedence
  and chained comparisons (`1 < x < 10`).

## Detailed design

### Crate-by-crate scope

#### `weavepy-lexer`

| In scope                                                                       | Deferred                                                |
|--------------------------------------------------------------------------------|---------------------------------------------------------|
| Full `TokenKind` aligned with CPython's `Lib/token.py`                         | f-string interior tokens (`FSTRING_START`/`MIDDLE`/`END`) |
| All 35 hard + 4 soft keywords                                                  | PEP 263 encoding declarations (default to utf-8)        |
| Integer literals: decimal, `0x`, `0o`, `0b`, with `_` separators               | `j` complex-number suffix                               |
| Float literals: with exponents, leading/trailing dots                          |                                                         |
| Strings: regular / raw / bytes / triple-quoted, escape sequences               | f-string interpolation (lexed as plain string; parser rejects with diagnostic pointing here) |
| Every operator and punctuation kind                                            |                                                         |
| Indent stack with `INDENT` / `DEDENT`; mixed tab/space rejection               |                                                         |
| Logical vs trivia newlines (`NEWLINE` vs `NL`)                                 |                                                         |
| Implicit line continuation inside `()` `[]` `{}` + explicit `\`                |                                                         |
| Comments (as trivia)                                                           |                                                         |

#### `weavepy-parser`

| In scope                                                                       | Deferred                                          |
|--------------------------------------------------------------------------------|---------------------------------------------------|
| `Module`, `FunctionDef`, `Return`, `Assign`, `AugAssign`, `AnnAssign`          | `Match` / `Case`                                  |
| `If`/`Elif`/`Else`, `While`, `For`, `Break`, `Continue`, `Pass`                | `Async` / `Await` / `Yield` / `YieldFrom`         |
| `Import`, `ImportFrom`, `Global`, `Nonlocal` *(execution: RFC 0012)*           | f-string interior parsing                         |
| `ClassDef`, `Try` / `Except` / `Finally`, `Raise`, `With` *(RFC 0003 / 0004)*  |                                                   |
| Full expression grammar with correct precedence                                |                                                   |
| Chained comparisons (`1 < x < 10`)                                             |                                                   |
| `BinOp`, `BoolOp`, `UnaryOp`, `Compare`, `IfExp`                               |                                                   |
| `Lambda`, `NamedExpr` (walrus), `Starred`                                      |                                                   |
| Calls with positional / keyword / `*args` / `**kwargs` / pos-only / kw-only    |                                                   |
| `Attribute`, `Subscript`, `Slice`                                              |                                                   |
| `List` / `Tuple` / `Dict` / `Set` literals                                     |                                                   |
| `ListComp` / `SetComp` / `DictComp` / `GeneratorExp`                           |                                                   |
| Decorators on function and class definitions *(RFC 0003)*                      |                                                   |

AST node shape mirrors CPython's `ast` module — same field names, same
nesting — so `ast.dump` of equivalent inputs matches.

#### `weavepy-compiler`

| In scope                                                                       | Deferred                                          |
|--------------------------------------------------------------------------------|---------------------------------------------------|
| `CodeObject` with deduplicated constants pool, names, varnames, freevars, cellvars | Line tables / exception tables (placeholders) |
| Module-level + nested function compilation                                     | Specialization / quickening                       |
| `LOAD_CONST` / `LOAD_NAME` / `LOAD_GLOBAL` / `LOAD_FAST` / `STORE_*` / `DELETE_*` |                                                |
| `LOAD_DEREF` / `STORE_DEREF` / `MAKE_CELL` for closures                        |                                                   |
| `BINARY_OP` (sub-op matches CPython 3.11+), `COMPARE_OP`, `UNARY_*`, `IS_OP`, `CONTAINS_OP` |                                       |
| `CALL` / `RETURN_VALUE` / `RESUME`                                             |                                                   |
| `POP_JUMP_IF_FALSE`/`TRUE`, `JUMP_FORWARD`/`BACKWARD`, `GET_ITER`/`FOR_ITER`/`END_FOR` |                                           |
| `BUILD_LIST` / `BUILD_TUPLE` / `BUILD_MAP` / `BUILD_SET` / `LIST_APPEND` / `SET_ADD` / `MAP_ADD` |                                  |
| `MAKE_FUNCTION` with defaults / kw-defaults / annotations slots                |                                                   |
| Comprehensions lowered to anonymous functions (matches CPython)                |                                                   |
| `CodeObject::format_dis()` so the `dis` conformance phase goes live            |                                                   |

#### `weavepy-vm`

| In scope                                                                       | Deferred                                          |
|--------------------------------------------------------------------------------|---------------------------------------------------|
| `Frame` with locals + eval stack + code + `f_back`                             | Generators / coroutines                           |
| Frame stack with push/pop on call/return                                       | Exceptions (`Result<_, PyException>` plumbing is in place; surface APIs deferred until try/except) |
| Object model: cheap-to-clone `enum Object` over `Rc<…>` (v1 — known to need redesign; gets its own RFC after) | Real GC (cycles deferred; Rc is fine for the slice) |
| Types: `None`, `bool`, `int` (i64 with overflow `TODO`), `float`, `str`, `list`, `tuple`, `dict`, `range`, function, method, builtin | Arbitrary-precision int |
| Method dispatch via type slot table (`str.upper`, `list.append`, `dict.get`, `list.__iter__`) | Full data model (`__add__` user overloads etc.) |
| Builtins: `print`, `len`, `range`, `str`, `int`, `float`, `bool`, `list`, `tuple`, `dict`, `type`, `repr`, `abs`, `min`, `max`, `sum`, `sorted`, `enumerate`, `zip`, `map`, `filter`, `all`, `any`, `isinstance`, `bool` | Full `sys` / `os` / etc. |
| Configurable stdout on `Interpreter` (keeps the **embeddable** goal honored)   | C-API                                             |
| Truthiness via type slot                                                       | REPL                                              |
| Comparison semantics including chained `1 < 2 < 3` and Python equality rules   |                                                   |

### Object model (v1)

`Object` is a cloneable enum behind `Rc<…>` for heap variants:

```rust
pub enum Object {
    None,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(Rc<str>),
    Tuple(Rc<[Object]>),
    List(Rc<RefCell<Vec<Object>>>),
    Dict(Rc<RefCell<DictData>>),
    Range(Rc<Range>),
    Function(Rc<PyFunction>),
    Builtin(Rc<BuiltinFn>),
    BoundMethod(Rc<BoundMethod>),
    Type(Rc<TypeObject>),
    Code(Rc<CodeObject>),
    Cell(Rc<RefCell<Object>>),
    Iter(Rc<RefCell<Iterator_>>),
}
```

- `is` uses `Rc::ptr_eq` for heap variants and value equality for the
  small-int / bool / `None` family — matching CPython's "small int cache"
  identity guarantee for the small ints we care about.
- `==` is value-based and recursive.
- This is a placeholder. **RFC 0002** will redesign the object model
  (tagged pointers / NaN-boxing / proper type slot table) after the
  slice has run real programs through this representation.

### Bytecode (v1)

CPython 3.13 uses a 16-bit-wide instruction stream with `EXTENDED_ARG`
for wide operands. The slice uses a simpler `{ op: OpCode, arg: u32 }`
struct per instruction — not byte-compact, but trivially fast to emit
and dispatch. The opcode *names* match CPython's so `dis` output stays
recognisable. A wire-compatible 16-bit encoding can be a perf-oriented
follow-up.

### Errors

Errors flow as Rust `Result`s. Each phase has its own error type
(`LexError`, `ParseError`, `CompileError`, `RuntimeError`) and the
umbrella `weavepy::Error` collapses them. `RuntimeError::PyException`
carries a `PyException` struct (type name + message) so the eventual
`try`/`except` work can map directly to it.

## Drawbacks

- **Size**. ~8–15 kloc landed at once is a large commit to review. We've
  weighed this against the alternative — landing a five-PR stack —
  and accepted the single-commit cost in exchange for keeping the slice
  cohesive and validated end-to-end.
- **Object model rework looming**. The `Rc<enum>` representation is
  known to be a placeholder. We accept a future rewrite cost
  (RFC 0002) in exchange for getting Python running this quarter.
- **i64 ints, not bignums**. Code that exceeds `i64::MAX` is a
  documented limitation today. Tracked; not blocking.

## Alternatives

- **Build one phase at a time to "perfection" before moving on.** The
  classic failure mode — phases never get end-to-end feedback. Rejected.
- **Land a tiny tracer bullet (`print(1 + 2)` only) and grow.** Earlier
  recommendation. Better for review, but the user explicitly chose the
  larger scope to avoid a long series of small follow-ups before
  WeavePy could do anything interesting.
- **Use a parser generator (LALRPOP, pest, tree-sitter).** Faster to
  bootstrap but commits us to that tool's error reporting model. A
  hand-written recursive-descent parser is what CPython itself uses
  (since 3.9 / PEP 617) and gives us full control over diagnostics.

## Prior art

- **CPython 3.13**: the spec. Hand-written PEG-ish parser, stack VM,
  16-bit bytecode, adaptive specialization.
- **PyPy**: tracing JIT over an interpreter written in RPython. Our
  performance roadmap intentionally borrows their tiered model.
- **RustPython**: similar design space (Rust-hosted Python). They
  vendor much of CPython's stdlib and run CPython tests under their
  interpreter — the model our conformance harness's Stage B will adopt.

## Unresolved questions

- Exact `is` semantics for medium-sized integers. CPython caches
  `-5..=256` as immortal singletons; we get the same observable
  behavior with our value-based `Int(i64)` equality, but a future
  object model with real heap-allocated ints will need explicit
  caching.
- Whether `BINARY_OP` should carry CPython's full sub-op enum or a
  smaller WeavePy-specific one. Today: match CPython, keep
  conformance high.

## Future work

- **RFC 0002**: object model and value representation (tagged pointers,
  NaN-boxing, type slot table).
- **RFC 0003**: classes, methods, MRO, descriptors. ✅ landed.
- **RFC 0004**: exceptions, `try` / `except` / `finally` / `with`, traceback. ✅ landed.
- **RFC 0005**: f-strings — PEP 701 lexer mode, `FSTRING_*` tokens,
  AST `JoinedStr` / `FormattedValue`.
- **RFC 0006**: generators / coroutines / async / await.
- **RFC 0007**: bytecode compaction (16-bit encoding + `EXTENDED_ARG`).
- **RFC 0008**: arbitrary-precision integers.
- **RFC 0012**: modules, the import system, and a minimal stdlib bootstrap
  (`sys` / `math` / `os` / `os.path`). ✅ landed.
