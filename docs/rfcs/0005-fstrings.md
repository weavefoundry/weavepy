# RFC 0005: Formatted string literals (f-strings)

- **Status**: Accepted
- **Authors**: WeavePy authors
- **Created**: 2026-05-21
- **Tracking issue**: TBD

## Summary

Implement Python's formatted string literals (f-strings) end-to-end —
parsing, AST shape, bytecode, and runtime formatting. After this RFC
lands:

- `f"x={x}"`, `f"{a + b:.2f}"`, `f"{x!r:>10}"`, `f"{'a' if c else 'b'}"`,
  `f"{x = }"` all work and produce the same strings CPython does.
- `JoinedStr` and `FormattedValue` AST nodes appear in `ast.dump` output
  in the same shape as CPython's.
- New `FORMAT_VALUE` bytecode runs the format-spec mini-language
  (`[[fill]align][sign][#][0][width][,][.precision][type]`) on the value
  on the stack, alongside the existing `BUILD_STRING` opcode.
- A new `format()` builtin invokes the same formatter so handwritten
  `format(x, '.2f')` produces identical output to `f"{x:.2f}"`.

This closes the largest single parser hole on the path to "drop-in
replacement for CPython": modern Python source uses f-strings
ubiquitously, and a parse-time `ParseError::NotImplemented` on the
first `f"..."` is what blocks most real-world files today.

## Motivation

Greenfield Python files written since 2018 use f-strings the way
earlier files used `%` formatting and `str.format`: they're the
default. A representative random sample from PyPI shows >80% of
modern files containing at least one f-string. As long as the
parser hard-errors on `f"..."`, WeavePy can run almost no real
Python source without manual rewriting.

f-strings are also a prerequisite for the upcoming standard-library
bootstrap RFC: vendoring `collections.py`, `pathlib.py`, `dataclasses.py`,
and friends from CPython is dramatically easier when we don't have to
rewrite their f-string usage first.

## CPython reference

This RFC tracks **CPython 3.13** but deliberately uses **CPython 3.11's
implementation strategy** as the reference rather than 3.12+'s
PEP 701 lexer rewrite. The motivation is purely engineering:

- PEP 701 makes f-strings tokenize in a stack-based lexer mode with
  `FSTRING_START` / `FSTRING_MIDDLE` / `FSTRING_END` tokens. That
  unlocks nested same-quote f-strings (`f"{f'{x}'}"`) and backslashes
  inside replacement fields, but at the cost of roughly 2 K LOC of
  lexer logic that's notoriously hard to get right.
- The CPython 3.11 strategy is simpler: lex an f-string as a single
  `STRING` token, then re-parse the body inside the parser. This
  handles the >99% common case (no nested f-strings, no backslashes
  in `{...}`) with a fraction of the code.

We track the **AST and runtime semantics** of 3.13 faithfully, but
the **tokenizer strategy** is 3.11-style. This is documented as a
known limitation; PEP 701-style lexing is a follow-up that can land
without changing the AST or bytecode shape.

Primary references:

- `Lib/ast.py` and `Python/Python-ast.c` — `JoinedStr` and
  `FormattedValue` node shapes.
- `Python/compile.c` — `FORMAT_VALUE`, `BUILD_STRING` emission.
- `Objects/unicodeobject.c` — `PyUnicode_Format` and friends.
- `Python/formatter_unicode.c` — the format-spec mini-language for
  the numeric and string types.

## Detailed design

### Lexer

No change to the token stream shape: `f"..."`, `f'''...'''`, `F"..."`,
`rf"..."`, `fr"..."` all continue to tokenize as a single
`TokenKind::String` with `StringPrefix::fstring = true`. The lexer
does need one micro-change: brace-pair tracking so that `f"{ {1:2} }"`
(an f-string containing a dict literal) isn't terminated by the
inner `}`. The existing scanner already tracks `paren_depth` for
implicit line continuation, but it doesn't track depth inside
string contents — luckily we don't *need* it to, because the f-string
is still emitted as one token; only the parser's re-lex of the
interior needs to track brace depth.

### AST additions

Two new `ExprKind` variants, mirroring CPython 1:1:

```rust
ExprKind::JoinedStr(Vec<Expr>),
ExprKind::FormattedValue {
    value: Box<Expr>,
    /// 0 = none, 's' = !s, 'r' = !r, 'a' = !a.
    conversion: i32,
    /// `None` for `{x}`; `Some(JoinedStr([...]))` for `{x:spec}`.
    format_spec: Option<Box<Expr>>,
},
```

`dump_module` extends to render these as
`JoinedStr(values=[...])` and
`FormattedValue(value=..., conversion=N, format_spec=...)`.

### Parser

When `decode_string` encounters an f-string prefix, dispatch to a new
`parse_fstring` helper:

1. Strip the prefix and the surrounding quotes (single or triple).
2. Walk the body byte-by-byte. Build up a `String` buffer for literal
   chunks; emit `Constant::Str` parts and reset.
3. On `{{`, emit a literal `{`; on `}}`, emit a literal `}`.
4. On `{`, switch to "field" mode: scan ahead to the matching `}`,
   tracking nested `(){}[]` and string literals. The slice between
   `{` and `}` is the field contents.
5. Re-lex the field with `weavepy_lexer::tokenize` and re-parse it
   with the expression parser to produce the `value` `Expr`. Detect
   trailing `!s` / `!r` / `!a` for `conversion`; detect a trailing
   `:` for `format_spec` (which is itself a `JoinedStr` recursion —
   format specs can contain interpolated values, e.g. `{x:{width}}`).
6. Detect `{x = }` debug form (PEP 614 / Python 3.8): expand to
   `"x = " + repr(x)` semantics by emitting a literal `"x = "` then
   the value with `conversion = 'r'`.
7. Wrap the assembled parts in `ExprKind::JoinedStr`. If the entire
   f-string is empty (`f""`), emit a single empty `Constant::Str`.

The recursive re-lex/re-parse is bounded: format specs nest at most
twice in practice (a `Constant::Str` in a `JoinedStr` in a
`FormattedValue.format_spec`).

Adjacent-string concatenation still works at the expression level:
`f"x" "y"` produces a `JoinedStr` whose first chunk is `"xy"`. CPython
collapses these via the parser pass; we do the same in `parse_atom`'s
existing concatenation loop.

### Compiler

The compiler emits the standard CPython sequence:

```
LOAD_CONST  "literal1"
LOAD_FAST   x
FORMAT_VALUE conversion_flags     ; runs format(), optional !s/!r/!a + spec
LOAD_CONST  "literal2"
BUILD_STRING 3                    ; joins N parts off the stack
```

The `FORMAT_VALUE` argument is CPython's bitmask:

| bit | meaning |
|-----|---------|
| 0x03 | conversion: 0 = none, 1 = `!s`, 2 = `!r`, 3 = `!a` |
| 0x04 | a format spec follows on the stack as a string |

For `{x:{width}}` the spec is itself a `BUILD_STRING` produced just
before the `FORMAT_VALUE`. This matches CPython byte-for-byte.

`BUILD_STRING` already exists in the slice but was never emitted;
this RFC starts emitting it.

### VM

`FORMAT_VALUE` calls a new `format_value` helper that mirrors
`PyObject_Format`:

1. Apply the conversion (`str`, `repr`, `ascii`) if requested.
2. If a `format_spec` was pushed, pop it; otherwise use `""`.
3. Look up `__format__` on the value's type. Built-in types route
   to a hand-written formatter; instances dispatch through the
   normal dunder protocol.

The built-in formatter handles the full format-spec mini-language:

```
format_spec ::= [[fill]align][sign][#][0][width][,_][.precision][type]
fill        ::= <any character>
align       ::= "<" | ">" | "=" | "^"
sign        ::= "+" | "-" | " "
width       ::= digit+
precision   ::= digit+
type        ::= "b" | "c" | "d" | "e" | "E" | "f" | "F" | "g" | "G"
              | "n" | "o" | "s" | "x" | "X" | "%"
```

Coverage in this RFC:

- Integers: `b`, `c`, `d` (default), `o`, `x`, `X`, plus width / fill /
  align / sign / grouping (`,` and `_`).
- Floats: `e`, `E`, `f`, `F`, `g`, `G`, `%`, default — width / precision /
  fill / align / sign.
- Strings: `s` (default), width / fill / align (`=` is rejected for
  strings to match CPython).
- Bools and `None` reuse the string path via `str(value)`.

Out of scope: `n` (locale-aware), arbitrary-precision int formatting
beyond `i64::MAX`, and the `complex` and `Decimal` types (RFC 0008).

### `format()` builtin

A new top-level `format(value, spec="")` builtin reuses the same
formatter. Its docstring matches CPython: "Return value.\_\_format\_\_(spec)."

### Errors

| Source error | Maps to |
|--------------|---------|
| Mismatched `{` / `}` in body | `SyntaxError` (currently `ParseError::Unexpected`) |
| Unknown conversion (e.g. `!q`) | `SyntaxError` |
| Unknown format type (e.g. `{x:q}`) | `ValueError` at runtime |
| `f"{x:>{w}}"` where `w` isn't an int | `TypeError` at runtime |

## Drawbacks

- **PEP 701 not fully tracked.** Same-quote nested f-strings
  (`f"{f'{x}'}"`) and backslashes inside replacement fields
  (`f"{x\n}"`) are rejected with a clear diagnostic pointing at
  the upgrade RFC. This affects a tiny fraction of real code; we
  accept the gap for now.
- **No fast path for "no interpolations".** Plain `f"hello"` is
  compiled identically to `"hello"`, but `f"{x}"` always goes
  through `FORMAT_VALUE` + `BUILD_STRING` even when CPython 3.12
  would emit just a `LOAD_FAST` + `LOAD_CONST` joined construction.
  Tracked but not blocking.

## Alternatives

- **Full PEP 701 lexer.** Considered and rejected for this RFC.
  Buys very little behavioural coverage at significant complexity
  cost.
- **Re-emit `%` and `str.format` desugaring.** CPython doesn't, so
  we don't either — the `dis` output would diverge.

## Prior art

- **CPython 3.11**: the reference implementation strategy we copy.
- **PyPy**: same strategy.
- **RustPython**: implements PEP 701 in full and incurs the
  associated lexer complexity. Cited as the upgrade path when this
  RFC's tokenization story is revisited.

## Unresolved questions

- Whether `{x = }` debug-mode formatting should preserve the exact
  whitespace from source (CPython 3.8+) or normalize it. We pick
  CPython's "preserve" behaviour.

## Future work

- **RFC 0005-B**: PEP 701 stack-based lexer mode. Lifts the
  same-quote and backslash restrictions.
- **RFC 0008**: arbitrary-precision integers will extend the
  integer formatter beyond `i64`.
