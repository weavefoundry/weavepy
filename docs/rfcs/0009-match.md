# RFC 0009: Structural pattern matching (`match`/`case`)

- **Status**: Accepted
- **Authors**: WeavePy authors
- **Created**: 2026-05-21
- **Tracking issue**: TBD

## Summary

Implement PEP 634 / PEP 636 — structural pattern matching — across the
parser, AST, compiler, and runtime. After this RFC lands, every form
of `match`/`case` documented in the language reference works:

- Literal patterns: `case 0:` / `case "x":` / `case None:` / `case True:`.
- Value patterns: `case Color.RED:` (qualified names load values).
- Capture patterns: `case x:` binds the subject to `x`.
- Wildcard patterns: `case _:` matches anything, binds nothing.
- Sequence patterns: `case [a, b, *rest]:` and `case (a, b):`.
- Mapping patterns: `case {"name": n, **extra}:`.
- Class patterns: `case Point(x=0, y=y):` and positional via
  `__match_args__`.
- OR patterns: `case 0 | 1 | 2:`.
- AS patterns: `case [_] as singleton:`.
- Guard clauses: `case x if x > 0:`.

`match` and `case` remain **soft keywords**: they're identifiers
everywhere except at the start of a `match` statement / `case` clause,
exactly as CPython treats them.

## Motivation

`match` is the third major piece of modern Python syntax that the
slice currently can't parse. Adding it together with f-strings and
generators lands the trio that real codebases depend on for
readable, idiomatic data-shape handling:

- Compiler-style code (the WeavePy parser, ironically) is a natural
  fit — every recursive-descent helper that handles N constructor
  variants is one giant `match`.
- Protocol parsers, JSON walkers, AST visitors, and (yes) other
  Python interpreters all use `match` extensively in recent code.
- Tools like `mypy`, `ruff`, `black` have begun adopting `match`
  internally; vendoring them is blocked on this RFC.

The bytecode lowering is also unique among Python features in that
it adds genuinely new dispatch primitives (`MATCH_CLASS`,
`MATCH_MAPPING`, `MATCH_SEQUENCE`, `MATCH_KEYS`). Implementing them
once and reusing them for the related `isinstance`-fast-path
opcodes is cheaper than reinventing the wheel later.

## CPython reference

Tracks **CPython 3.13**:

- `Python/Python-ast.c` — `Match`, `match_case`, `MatchValue`,
  `MatchSingleton`, `MatchSequence`, `MatchMapping`, `MatchClass`,
  `MatchStar`, `MatchAs`, `MatchOr` node shapes.
- `Python/compile.c` — `compiler_match`, `compiler_pattern_*`.
- `Python/ceval.c` — `MATCH_CLASS`, `MATCH_MAPPING`,
  `MATCH_SEQUENCE`, `MATCH_KEYS`, `GET_LEN`.
- PEP 634 (Specification), PEP 635 (Motivation), PEP 636 (Tutorial).
- `Objects/object.c` — `Py_GenericAlias` is **not** needed here;
  class patterns work against ordinary types.

We intentionally do not track:

- Optimisation passes (CPython's compiler de-duplicates common
  subpatterns; we don't).
- `Final` / `frozen` dataclass interaction subtleties.

## Detailed design

### AST additions

A new statement kind plus a new pattern hierarchy:

```rust
pub enum StmtKind {
    // ...existing variants...
    Match {
        subject: Expr,
        cases: Vec<MatchCase>,
    },
}

pub struct MatchCase {
    pub pattern: Pattern,
    pub guard: Option<Expr>,
    pub body: Vec<Stmt>,
    pub span: Span,
}

pub enum Pattern {
    Value(Expr),                                     // 0, "x", Foo.RED
    Singleton(Constant),                             // None, True, False
    Capture(Option<String>),                         // x  |  _
    Sequence(Vec<Pattern>),                          // [a, b, *rest]
    Star(Option<String>),                            // *rest  or *_
    Mapping {
        keys: Vec<Expr>,
        patterns: Vec<Pattern>,
        rest: Option<Option<String>>,                // **rest
    },
    Class {
        cls: Expr,
        positionals: Vec<Pattern>,
        keywords: Vec<(String, Pattern)>,
    },
    Or(Vec<Pattern>),                                // a | b | c
    As {
        pattern: Box<Pattern>,
        name: String,
    },
}
```

`dump_module` extends to render every variant in CPython shape.

### Parser

`match` becomes a **soft keyword**. The parser dispatches into
`parse_match` only when it sees `match` at the start of a statement
and the next token *isn't* one that would make `match` an
ordinary identifier (`(`, `[`, `=`, etc.). We use CPython's same
heuristic: if `match <expr>:` followed by NEWLINE INDENT `case ...:`
parses, it's a match statement; otherwise treat `match` as a name.

The full pattern grammar is implemented in `parse_pattern_*` helpers:

```text
pattern        ::= or_pattern ['as' NAME]
or_pattern     ::= closed_pattern ('|' closed_pattern)*
closed_pattern ::= literal | name | wildcard | sequence
                 | mapping | class | parenthesized
literal        ::= NUMBER | STRING | 'None' | 'True' | 'False'
name           ::= NAME ('.' NAME)*           ; dotted = value, plain = capture
sequence       ::= '[' pat_list? ']' | '(' pat_list ')'
mapping        ::= '{' (key_value (',' key_value)*)? '}'
class          ::= name '(' pat_args? ')'
pat_args       ::= pattern (',' pattern)* (',' kw_pat (',' kw_pat)*)?
                 | kw_pat (',' kw_pat)*
kw_pat         ::= NAME '=' pattern
```

The "dotted name" rule is what distinguishes a *value* pattern
(`case Color.RED:`) from a *capture* pattern (`case red:`): if the
identifier path contains a `.`, it's a value lookup.

### Compiler

New opcodes:

```rust
OpCode::MatchSequence,  // pushes True if TOS is a sequence (not str/bytes)
OpCode::MatchMapping,   // pushes True if TOS is a mapping
OpCode::MatchClass,     // arg = positional count; pops 3, pushes match tuple or None
OpCode::MatchKeys,      // pop key tuple, peek mapping; push value tuple or None
OpCode::GetLen,         // peek TOS, push len(TOS)
```

The full match lowering follows CPython's recipe:

1. Evaluate the subject, store in a synthetic local `.match0`.
2. For each case:
   - Load subject onto stack (multiple times as needed).
   - Emit pattern-test opcodes. On mismatch, jump to the next case.
   - If a guard is present, evaluate and `POP_JUMP_IF_FALSE` to the
     next case.
   - Run the body. `JUMP_FORWARD` past the remaining cases.
3. If no case matches, fall through (match statements have no
   implicit `else`).

Pattern-specific lowering, in shorthand:

- **Value pattern `V`**: `LOAD <V>; CompareOp Eq; PopJumpIfFalse next`.
- **Singleton `None`/`True`/`False`**: `LOAD_CONST X; IS_OP 0; PopJumpIfFalse next`.
- **Capture `x`**: `STORE_FAST x` (binding may be deferred until after
  guard succeeds).
- **Wildcard `_`**: `POP_TOP`.
- **Sequence `[p1, p2]`**: `MATCH_SEQUENCE` test, then `GET_LEN`/
  compare, then unpack & recurse.
- **Mapping `{k: p}`**: `MATCH_MAPPING` test, then `MATCH_KEYS` for
  the requested keys.
- **Class `C(a, b=p)`**: load `C`, push key tuple of declared kwargs,
  `MATCH_CLASS` with positional count; result is a tuple of attribute
  values or `None`. Continue recursion on each attribute.
- **Or `a | b | c`**: try each subpattern, falling through on success.
- **As `p as name`**: succeed `p`, then bind name.

CPython adds an "early-bind locals" pass that ensures all bindings
inside a successful case are visible after the case (via `STORE_FAST`
into the enclosing scope). We do the same.

### VM

New runtime helpers:

```rust
fn match_sequence(obj: &Object) -> bool { /* list, tuple, range, ... */ }
fn match_mapping(obj: &Object) -> bool { /* dict, instances of Mapping */ }
fn match_keys(map: &Object, keys: &[Object]) -> Option<Vec<Object>> { ... }
fn match_class(cls: &Object, subject: &Object, posc: usize,
               kwnames: &[Object]) -> Option<Vec<Object>> { ... }
```

`match_class` honours `__match_args__` (a class-level tuple of
attribute names) for positional sub-patterns and falls back to
ordinary `__getattr__` for keyword sub-patterns. Built-in types
declare their canonical `__match_args__` (`int`, `str`, etc. accept
exactly one positional that is the value itself; `list` and `dict`
fall through to the sequence/mapping matchers).

### Soft-keyword resolution

The lexer continues to emit `match` and `case` as `TokenKind::Name`
unless they're CPython-reserved keywords. We **do not** add them to
the `Keyword` enum — that would break legacy code that uses `match`
as an identifier (the `re.match` function being the canonical
example). The parser handles disambiguation entirely.

### Errors

| Source | Error |
|--------|-------|
| Star pattern at non-end-of-sequence | `SyntaxError` |
| Duplicate keyword in mapping pattern | `SyntaxError` |
| Bind a name twice across alternatives in `|` | `SyntaxError` |
| Non-string mapping key | `SyntaxError` |
| Class pattern target isn't a type | `TypeError` at runtime |

## Drawbacks

- **The pattern grammar is large** (≈300 LOC of parser code). Tested
  exhaustively in the conformance corpus.
- **`MATCH_CLASS` semantics differ slightly** between CPython 3.10 / 3.11 /
  3.12 / 3.13 around `__class_getitem__`-flavoured types. We track
  3.13 and document the deviations.
- **Slight extra cost on every statement parse.** `parse_statement`
  performs one extra lookahead for the soft-keyword check. Negligible
  in practice.

## Alternatives

- **Implement only `case <literal>:` and `case <name>:`**. Considered
  and rejected — the value comes from the whole feature, not a subset.
- **Lower `match` to chained `if isinstance ... and getattr ...`**.
  Cleaner-looking output but breaks `dis` conformance and is slower
  on deep patterns. Rejected.

## Prior art

- **CPython 3.10–3.13**: the canonical implementation we track.
- **MyPy**: implements pattern matching at the type level; useful
  reference for `__match_args__` semantics.
- **OCaml / Rust**: the language design lineage of structural
  matching. Not directly applicable but cited in PEP 635.

## Unresolved questions

- Whether `Object::Range` should match `MATCH_SEQUENCE`. CPython
  returns False (range is not registered as a sequence). We follow.

## Future work

- **Type-directed `__match_args__` inference** for dataclasses (we
  already populate `__match_args__` at decoration time when
  dataclasses land — RFC 0014).
- **PEP 695 / TypeAliasType pattern interactions** — RFC 0013.
