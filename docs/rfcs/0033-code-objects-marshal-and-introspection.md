# RFC 0033: CPython-faithful code objects, `marshal`/`.pyc`, and the introspection modules (`ast` / `dis` / `opcode` / `symtable`)

- **Status**: Accepted
- **Authors**: WeavePy authors
- **Created**: 2026-05-29
- **Tracking issue**: TBD
- **Builds on**: RFC 0001 (executable slice / bytecode), RFC 0019
  (`marshal` + serialization), RFC 0021 (inline caches — the `CACHE`
  pseudo-instructions `dis` must render), RFC 0031 (observability —
  precise positions feed tracebacks)
- **Relates to**: the long-reserved **RFC 0007** ("bytecode compaction:
  16-bit encoding + `EXTENDED_ARG`"). RFC 0033 delivers the *observable*
  CPython bytecode surface — `co_code`, `dis` output, `marshal`, `.pyc` —
  **without** re-encoding the VM's internal instruction stream. The
  internal re-encoding remains RFC 0007's job; see "Alternatives".

## Summary

WeavePy executes Python correctly but is **not introspectable like
CPython**. Four modules every serious tool reaches for are missing
outright — `import ast`, `import dis`, `import opcode`, and
`import symtable` all raise `ModuleNotFoundError` — `marshal` refuses to
serialize a code object, `.pyc` files use a private `b"WPY0"` magic with
a private payload, and the `code` object exposes none of CPython's
`co_*` surface (`co_code`, `co_linetable`, `co_exceptiontable`,
`co_positions()`, `co_qualname`, …). As a direct consequence the
conformance harness's `ast` and `dis` phases are permanently wired to
`Skipped` (see `docs/CONFORMANCE.md`), and the "Stage B regrtest" vision
the whole project is organized around cannot grade a single thing about
compiled output.

This RFC closes that gap with one coherent layer:

1. A **CPython-3.13 bytecode codec** (`weavepy-compiler::cpython_code`):
   a faithful encoder from WeavePy's internal `Vec<Instruction>` to
   CPython's 16-bit `_Py_CODEUNIT` stream — opcode mapping,
   `EXTENDED_ARG`, jump-offset reconversion, and `CACHE`-entry insertion
   so byte offsets and `dis` columns line up — plus a decoder for the
   canonical (non-adaptive) opcode set that `.pyc`/`marshal` actually
   carries.
2. The **PEP 626 location table** (`co_linetable`) and **PEP 657
   positions** (`co_positions()`): column tracking is threaded from the
   parser's byte `Span`s through the compiler into the code object, then
   encoded into CPython's varint location format.
3. The **CPython exception table** (`co_exceptiontable`): the existing
   structured `exception_table: Vec<ExcHandler>` is encoded into
   CPython's varint range table and decoded back.
4. A **CPython-compatible `marshal`** (version 4, with the `FLAG_REF`
   object-reference table) that serializes and deserializes `TYPE_CODE`
   in CPython's field order, and a **CPython-magic `.pyc`** (PEP 552
   timestamp + hash modes) that writes a real marshalled code object —
   *fixing the silent no-op `.pyc` writer that ships today*.
5. The four **introspection modules**: `opcode` (the canonical tables),
   `dis` (a faithful disassembler over the codec), `ast` (the parser AST
   surfaced to Python with `parse` / `dump` / `unparse` / `walk` /
   `NodeVisitor` / `NodeTransformer` and `compile(tree)`), and
   `symtable` (a scope analyzer over the same AST).
6. The **conformance harness** flips its `ast` and `dis` phases from
   `Skipped` to live diffs against `ast.dump` and `dis.dis`, and the
   regrtest allowlist gains `test_dis`, `test_marshal`, `test_ast`,
   `test_code`, and `test_compile`.

Net diff: **~22–30K LOC** (the codec, the location/exception encoders,
the four modules — `dis`/`ast`/`opcode` shipped as frozen Python over a
thin Rust core, `symtable` likewise — the `code`-object surface, the
`marshal`/`.pyc` work, the conformance wiring, fixtures, and tests).

Mission alignment is direct. The README's goal #1 is *"Compatibility
first … the reference C-API is the spec … `dis` output, `sys.implementation`,
`__pycache__` layout … all of it."* Today WeavePy mirrors the runtime
behavior but not the *artifacts*. After this RFC, `weavepy` and
`python3` agree on what a compiled function **looks like**, not just
what it **does**.

## Motivation

A drop-in replacement is judged on two axes: *does my code run?* and
*does my tooling work?* WeavePy is strong on the first and silently
broken on the second. Concretely, today:

```text
$ weavepy -c "import ast"        ->  ModuleNotFoundError: No module named 'ast'
$ weavepy -c "import dis"        ->  ModuleNotFoundError: No module named 'dis'
$ weavepy -c "import marshal; marshal.dumps(compile('1','<s>','eval'))"
      ->  ValueError: marshal: code objects are not yet serialisable
$ weavepy -c "import importlib.util as u; print(u.MAGIC_NUMBER.hex())"
      ->  57505930   (b"WPY0", not CPython's f30d0d0a)
```

The blast radius is large and load-bearing:

- **`ast` is imported by the tooling ecosystem's core.** `black`,
  `flake8`, `mypy`, `isort`, `bandit`, `pylint`, `attrs`, `pydantic`,
  and — most importantly for *this* project — **`pytest`'s assertion
  rewriting** all `import ast`. WeavePy ships a bundled `_pytest`
  (RFC 0030/0031), but a real third-party plugin or a user `conftest.py`
  that touches `ast` falls over. `ast` is arguably the single highest-
  traffic missing module in the tree.
- **`dis` is how people (and tests) inspect bytecode.** `test_dis.py`,
  IPython's `%%timeit`-adjacent introspection, teaching tools,
  decompilers, and coverage/debug tooling all decode `co_code`. With no
  `dis` and no CPython-shaped `co_code`, none of it runs.
- **`.pyc` writing is dead code right now.** `pycache::try_write`
  builds an `Object::Code`, calls `marshal_mod::b_dumps`, and that call
  **returns `Err`** (marshal rejects code objects) — so the
  `let Ok(payload) = … else { return; }` swallows it and **no `.pyc` is
  ever written**. Startup re-compiles every module on every run. Fixing
  marshal turns the existing, already-wired `__pycache__` machinery on.
- **The acceptance harness is blindfolded.** `docs/CONFORMANCE.md` lists
  the `ast` and `dis` phases as `Skipped` "until WeavePy emits
  comparable output." That sentence has been true for 32 RFCs. This is
  the RFC that makes it false — and a graded `dis`/`ast` diff is exactly
  the regression signal RFC 0007 (internal re-encoding) and every future
  compiler change will need.

Down-tree, this RFC unblocks:

- **Real `pytest` plugins and `conftest.py`** that import `ast`.
- **Fast startup** via a working `.pyc` cache.
- **`python -m compileall`, `py_compile`, `runpy` over `.pyc`**, and any
  workflow that ships pre-compiled bytecode.
- **The conformance `dis`/`ast` phases** and a regrtest baseline for
  compiled output, the prerequisite for grading RFC 0007.

## CPython reference

This RFC tracks **CPython 3.13** exactly. The governing references:

- **`Lib/opcode.py` + `Lib/_opcode_metadata.py`** — opcode numbers,
  `HAVE_ARGUMENT`, `hasjrel`/`hasjabs`/`hasconst`/`haslocal`/`hasname`/
  `hasfree`/`hascompare`, and `_inline_cache_entries` (the per-opcode
  `CACHE` counts the encoder must reproduce).
- **`Include/cpython/code.h`, `Objects/codeobject.c`** — the `co_*`
  fields, `co_localsplusnames` / `co_localspluskinds`
  (`CO_FAST_LOCAL`/`CO_FAST_CELL`/`CO_FAST_FREE`), `co_flags` bits
  (`CO_OPTIMIZED`, `CO_NEWLOCALS`, `CO_VARARGS`, `CO_VARKEYWORDS`,
  `CO_NESTED`, `CO_GENERATOR`, `CO_COROUTINE`, `CO_ASYNC_GENERATOR`),
  `co_stacksize`, `CodeType.replace()`, `co_lines()`, `co_positions()`.
- **PEP 3155** — `__qualname__` / `co_qualname`.
- **PEP 626** — precise line numbers; the `co_linetable` byte format
  (`InternalDocs/code_objects.md`, formerly `Objects/locations.md`).
- **PEP 657** — fine-grained error locations; `co_positions()` returns
  `(lineno, end_lineno, col_offset, end_col_offset)` per instruction.
- **`Python/marshal.c`** — the version-4 marshal format, `TYPE_*` tags
  (already mirrored in `marshal_mod.rs`), `FLAG_REF` (0x80) +
  `r_ref`/`w_ref` object-reference table, and the `TYPE_CODE` field
  order.
- **PEP 552** — deterministic, hash-based `.pyc` invalidation; the
  16-byte header (`magic`, `bit_field`, then either `(mtime, size)` or a
  64-bit source hash).
- **`Lib/importlib/_bootstrap_external.py`** — `MAGIC_NUMBER` (3.13 ⇒
  `3571`, serialized as `b"\xf3\x0d\x0d\x0a"`), `_code_to_*_pyc`,
  cache-tag (`cpython-313` ⇒ for us `weavepy-313`) path layout.
- **`Lib/dis.py`** — disassembly formatting (the exact column layout,
  `>>` jump targets, `CACHE` rendering under `show_caches`, the
  `--specialized` flag), `_parse_exception_table`, `findlabels`,
  `get_instructions`, `Instruction`/`Positions` namedtuples.
- **`Lib/ast.py`** — `parse`, `dump`, `literal_eval`, `walk`,
  `NodeVisitor`, `NodeTransformer`, `unparse`, `get_docstring`, and the
  `_ast` node-class hierarchy with `_fields` / `_attributes`.
- **`Lib/symtable.py` + `Symtable/symtable.c`** — `symtable.symtable()`,
  `SymbolTable`/`Function`/`Class`, `Symbol.is_local/.is_global/
  .is_free/.is_parameter`, nested-scope resolution.

The marshal `TYPE_CODE` field order we must match (3.13):

```text
argcount, posonlyargcount, kwonlyargcount, stacksize, flags,
code (TYPE_STRING bytes), consts, names, localsplusnames (tuple),
localspluskinds (TYPE_STRING bytes), filename, name, qualname,
firstlineno, linetable (TYPE_STRING bytes), exceptiontable (TYPE_STRING bytes)
```

## Detailed design

### Layering: a presentation codec, not an internal rewrite

The central design decision is that **the VM keeps running its own
`Vec<Instruction>`**. RFC 0021's inline caches, RFC 0031's hooks, and
RFC 0032's JIT all consume that representation; re-encoding it to 16-bit
`_Py_CODEUNIT`s under them is RFC 0007 and is explicitly out of scope.

Instead we add a **codec** that converts between the internal stream and
CPython's wire form *at the boundary* — when Python asks for `co_code`,
when `marshal`/`.pyc` serialize, and when `dis` disassembles. The
encoded form is computed lazily and memoized on the `CodeObject` (an
`OnceCell<CpythonCode>`), so the cost is paid once per code object that
is actually introspected and never on the hot execution path.

```rust
// crates/weavepy-compiler/src/cpython_code.rs  (new)

/// The CPython-3.13 wire view of a CodeObject. Derived on demand.
pub struct CpythonCode {
    pub co_code: Vec<u8>,             // packed _Py_CODEUNIT stream (LE)
    pub co_linetable: Vec<u8>,        // PEP 626 varint table
    pub co_exceptiontable: Vec<u8>,   // varint range table
    pub localsplusnames: Vec<String>, // varnames ++ cellvars ++ freevars
    pub localspluskinds: Vec<u8>,     // CO_FAST_* per localsplus entry
    pub flags: u32,                   // CO_* bitset
    pub stacksize: u32,               // computed by abstract stack-depth pass
    pub qualname: String,
    pub positions: Vec<Position>,     // (lineno,end_lineno,col,end_col) per instr
    pub co_code_map: Vec<u32>,        // CPython instr index -> internal instr index
}
```

`co_code_map` is the linchpin for the decoder/round-trip story (below).

### Part A — the bytecode codec (`weavepy-compiler`)

**Opcode mapping table.** A static table maps each WeavePy `OpCode` to
its CPython 3.13 `(opcode_number, cache_entry_count)`. Most are 1:1
(`LoadFast → LOAD_FAST`, `StoreFast → STORE_FAST`, `ReturnValue →
RETURN_VALUE`, `BinarySubscr → BINARY_SUBSCR`, …). A handful need
*expansion* or *argument re-encoding*:

| WeavePy op | CPython 3.13 emission | Notes |
|---|---|---|
| `BinaryOp(k)` | `BINARY_OP` arg=`k` + 1×`CACHE` | `BinOpKind` already matches `_nb_ops` index |
| `CompareOp(k)` | `COMPARE_OP` arg=`(k<<5)\|bit` + 1×`CACHE` | 3.13 packs the "convert to bool" bit; mask on read |
| `Call(n)` | `CALL` arg=`n` + 3×`CACHE` | `CallKw`→`KW_NAMES`+`CALL`; `CallEx`→`CALL_FUNCTION_EX` |
| `LoadGlobal` | `LOAD_GLOBAL` arg=`(i<<1)\|push_null` + 4×`CACHE` | low bit = "push NULL before" |
| `LoadAttr` | `LOAD_ATTR` arg=`(i<<1)\|method` + 9×`CACHE` | low bit distinguishes method loads |
| `JumpForward`/`JumpBackward`/`PopJumpIf*` | rel jump in **code units** | recomputed after CACHE insertion |
| `FormatValue` | `FORMAT_VALUE` (+`CONVERT_VALUE`/`FORMAT_SIMPLE` shape) | conversion/spec bits remapped |
| `Resume` | `RESUME` arg=0 | already present |

A small set of WeavePy helper opcodes have no exact 3.13 twin; each gets
a documented, behavior-preserving expansion to the nearest CPython
sequence, and the cases where a clean mapping is impossible are recorded
in a `DIVERGENCES.md`-style table and surfaced as a `# WEAVEPY:`
annotation in `dis` output so nothing is silently misrepresented.

**Encoding pass.** Two linear passes over the internal stream:

1. *Layout*: walk instructions, look up each one's `(op, ncache)`, and
   assign every internal instruction a CPython **code-unit offset**
   (1 unit for the instruction + `ncache` units + 1 unit per
   `EXTENDED_ARG` needed for args > 0xFF). Build `instr_index ->
   codeunit_offset`.
2. *Emit*: for each instruction, emit `EXTENDED_ARG`s (high bytes
   first), the opcode byte + low arg byte, then `ncache` zeroed `CACHE`
   units. Jump args are recomputed as `(target_offset - (this_offset +
   1 + ncache))` for relative jumps, in code units, matching CPython.

**Stack-depth pass.** CPython stores `co_stacksize`. We compute it with
a standard abstract-interpretation max-depth walk over the internal
stream (per-opcode push/pop deltas, taking the max over branches), which
also validates the bytecode is stack-balanced (a cheap correctness net
that has already caught compiler bugs in other implementations).

**Decoder.** `decode(co_code, consts, names, localsplus, …) ->
Vec<Instruction>` inverts the encoding for the **canonical, non-adaptive**
opcode set. This is sufficient for `.pyc`/`marshal` because CPython only
ever serializes canonical bytecode — specialization and quickening
happen at runtime in `co_code_adaptive` and are **never marshalled**. The
decoder skips `CACHE` units, folds `EXTENDED_ARG`, and reconverts
code-unit jump offsets back to internal instruction indices. Decoding
WeavePy's *own* emitted bytecode is total; decoding arbitrary
third-party CPython `.pyc` is best-effort and grows with opcode
coverage (see "Future work").

### Part B — locations: `co_linetable` (PEP 626) and `co_positions()` (PEP 657)

Today `CodeObject.linetable: Vec<u32>` carries **one line number per
instruction** and **no columns**. CPython needs full `(lineno,
end_lineno, col_offset, end_col_offset)` per instruction, encoded in the
compact PEP 626 byte format.

- **Column plumbing.** The parser AST already carries a byte `Span`
  (`ast.rs`). We thread `Span` through the compiler so every emitted
  `Instruction` records the source span of the expression/statement that
  produced it (a parallel `Vec<Span>`, same length as `instructions`).
  A `LineIndex` over the source maps `BytePos -> (line, col)` (UTF-8
  aware, col in code points to match CPython), giving all four position
  fields.
- **Encoding.** A `linetable` encoder emits the PEP 626 variable-length
  entries (the `PY_CODE_LOCATION_INFO_*` forms: short, one-line, no-
  column, and long), and `co_positions()` is decoded back from it for
  the Python surface. `co_lines()` (the `(start, end, line)` iterator)
  falls out of the same table.
- **Tracebacks.** RFC 0031's traceback rendering switches to the new
  position data, so `^^^^` carets (PEP 657) appear in WeavePy
  tracebacks — a visible, free win.

This is the **highest-risk sub-area** (column fidelity is exacting and
the AST→bytecode span attribution must be principled), so it ships with
a dedicated differential fixture set graded against `co_positions()`.

### Part C — the exception table: `co_exceptiontable`

`CodeObject.exception_table: Vec<ExcHandler>` is already structured
(start/end PC, handler PC, stack depth, lasti flag). We add:

- an **encoder** to CPython's varint range-table format (`Lib/dis.py
  _parse_exception_table` is the read side we mirror), with PCs
  converted to code-unit offsets via the Part A layout map, and
- a **decoder** for round-tripping.

`dis(show_caches=…)` renders the `ExceptionTable` section exactly as
CPython does.

### Part D — the `code` object surface (`weavepy-vm`)

The `Object::Code` type gains CPython's read-only attributes, each
backed by the memoized `CpythonCode` (computed on first access):

`co_argcount`, `co_posonlyargcount`, `co_kwonlyargcount`, `co_nlocals`,
`co_stacksize`, `co_flags`, `co_code` (bytes), `co_consts` (tuple),
`co_names` (tuple), `co_varnames`, `co_cellvars`, `co_freevars`,
`co_filename`, `co_name`, `co_qualname`, `co_firstlineno`,
`co_linetable` (bytes), `co_exceptiontable` (bytes), plus the methods
`co_positions()`, `co_lines()`, and `replace(**kwargs)`. `hash`/`==`
follow CPython (structural over the wire fields). These are exposed
through the existing attribute-dispatch path used for other builtin
types; no new object-model machinery is required.

### Part E — `marshal` code objects + CPython `.pyc` (`weavepy-vm`)

**`marshal`.** `marshal_mod.rs` already implements the v4 value format
with CPython's `TYPE_*` tags. We extend it:

- Implement `TYPE_CODE` (`'c'`) write/read using the field order above,
  driving `co_code`/`co_linetable`/`co_exceptiontable`/`localsplus*`
  from Part A–C and reconstructing a `CodeObject` via the Part A
  decoder on read.
- Add the **`FLAG_REF` object-reference table** (`w_ref`/`r_ref`) so
  shared/interned objects (notably nested code objects, names, and
  interned strings) round-trip by reference — required for byte-level
  parity and for `test_marshal`'s identity checks.
- Replace the approximate bigint packing with CPython's exact 15-bit
  (`PyLong_SHIFT`) digit layout so `TYPE_LONG` bytes match.
- `marshal.version` stays `4`.

**`.pyc`.** `pycache.rs` switches `MAGIC` to CPython's 3.13 value
(`b"\xf3\x0d\x0d\x0a"`, surfaced via `imp.get_magic()` /
`importlib.util.MAGIC_NUMBER`), keeps the PEP 552 16-byte header (adding
the hash-based mode for `--invalidation-mode checked-hash/unchecked-hash`),
writes a **real marshalled code object** (fixing the silent no-op), and
reads CPython-magic `.pyc` files back through the Part A decoder. The
cache tag becomes `weavepy-313` so WeavePy and CPython artifacts coexist
in one `__pycache__` without collision, matching the
`sys.implementation.cache_tag` contract.

### Part F — the four introspection modules

To keep the diff reviewable and the behavior faithful, `dis`, `ast`, and
`opcode` ship as **frozen Python** (vendored/adapted from CPython 3.13's
own `Lib/`) sitting on a **thin Rust core** that provides the data the
pure-Python layer needs. This mirrors how the project already ships
`pickle`, `argparse`, `inspect`, etc. as frozen Python.

- **`opcode`** (frozen `opcode.py` + `_opcode` Rust core): `opname`,
  `opmap`, `HAVE_ARGUMENT`, `EXTENDED_ARG`, the `has*` lists, `stack_effect()`,
  and `_inline_cache_entries`. The Rust `_opcode` core exposes
  `stack_effect` and the cache-entry table generated from Part A's
  mapping so there is a single source of truth.
- **`dis`** (frozen `dis.py`): consumes `co_code` + `co_*` and the
  `opcode` tables. Because Parts A–D make those CPython-shaped,
  upstream `dis.py` runs essentially unmodified — the strongest possible
  fidelity guarantee. `dis.dis`, `Bytecode`, `get_instructions`,
  `findlinestarts`, `show_caches`, and exception-table rendering all work.
- **`ast`** (`_ast` Rust core + frozen `ast.py`): the Rust core walks
  the existing parser `Module`/`Stmt`/`Expr` tree and builds Python
  `_ast.*` node objects (with `_fields`, `_attributes`, and `lineno`/
  `col_offset`/`end_lineno`/`end_col_offset` from the `LineIndex`).
  `ast.parse(src)` calls the parser with `PyCF_ONLY_AST`; `compile(tree,
  …)` accepts an `_ast.Module` and lowers it back through the compiler
  (an AST→AST bridge: Python `_ast` → Rust AST → bytecode).
  `ast.dump`, `walk`, `iter_child_nodes`, `NodeVisitor`,
  `NodeTransformer`, `literal_eval`, `get_docstring`, and `unparse` come
  from frozen `ast.py`. `ast.dump` parity is graded by the conformance
  harness.
- **`symtable`** (`_symtable` Rust core + frozen `symtable.py`): a scope
  pass over the Rust AST classifying each name as local / global
  (explicit/implicit) / free / cell / parameter, exposed via the
  CPython `SymbolTable`/`Symbol` surface. The compiler already performs
  scope analysis to populate `varnames`/`freevars`/`cellvars`; this
  factors that logic into a reusable analyzer feeding both the compiler
  and `symtable`.

### Part G — conformance + regrtest

- `weavepy-conformance` flips the **`ast` phase** (diff WeavePy
  `ast.dump(ast.parse(src))` vs the oracle) and the **`dis` phase**
  (diff `dis.Bytecode(...).dis()` vs the oracle) from `Skipped` to live.
  `docs/CONFORMANCE.md`'s "Where we are today" table is updated.
- New bundled regression tests: `test_code_object_surface.py`,
  `test_dis_dropin.py`, `test_marshal_roundtrip.py`,
  `test_pyc_roundtrip.py`, `test_ast_dropin.py`,
  `test_symtable_dropin.py`.
- `expectations.toml`: `test_dis`, `test_marshal`, `test_ast`,
  `test_code`, `test_compile`, `test_symtable` move toward `pass` (those
  whose remaining failures are unrelated long-tail keep a documented
  `fail` with a narrowed reason).
- The README "Status" paragraph gains a sentence: the `dis`/`ast`
  conformance phases are live and `.pyc`/`marshal` are CPython-wire-
  compatible for WeavePy-compiled code.

### Affected crates

- **`weavepy-compiler`** — new `cpython_code` module (codec, location
  encoder, exception encoder, stack-depth pass); `CodeObject` gains the
  memoized `CpythonCode` + a parallel `spans: Vec<Span>`; the compiler
  threads spans and reuses the factored scope analyzer.
- **`weavepy-vm`** — `marshal_mod` (`TYPE_CODE` + `FLAG_REF`),
  `pycache` (CPython magic + hash mode + real write), the `code`-object
  attribute surface, new `_opcode`/`_ast`/`_symtable` Rust cores, frozen
  `opcode.py`/`dis.py`/`ast.py`/`symtable.py`, traceback caret rendering.
- **`weavepy-parser`** — no shape change; the AST→`_ast` bridge reads it.
- **`weavepy-conformance`** — `ast`/`dis` phases go live; new fixtures.

### Performance assumptions

The codec is **off the hot path by construction**: it runs only when
`co_code`/`marshal`/`dis` is requested, and memoizes. Execution speed is
unchanged (verified by the RFC 0021/0032 bench suite as a no-regression
gate). The one always-on cost is the compiler now recording a `Span` per
instruction (one extra `Vec<Span>` per code object, populated during
emission); this is `O(instructions)` memory and zero extra time on
execution. `.pyc` going from "never written" to "written once" makes
*startup faster*, not slower, on the second run.

## Drawbacks

- **Two bytecode representations.** The internal `Vec<Instruction>` and
  the CPython wire form must stay semantically in lockstep. The codec is
  the single bridge and is differentially tested against the oracle, but
  it is genuine surface area. (RFC 0007 eventually collapses this by
  making the internal form *be* the wire form.)
- **The location table is fiddly.** PEP 626/657 encoding has several
  variable-length forms and exacting column semantics; getting `^^^^`
  carets and `co_positions()` byte-identical is the bulk of the risk and
  the test budget.
- **Best-effort foreign `.pyc`.** We can read back our own `.pyc` and
  CPython's canonical bytecode for implemented opcodes, but executing an
  *arbitrary* third-party `.pyc` compiled by CPython depends on full
  opcode-decoder coverage, which lands incrementally. We document the
  boundary rather than over-promise.
- **Frozen-stdlib drift.** Vendoring `dis.py`/`ast.py`/`symtable.py`
  pins them to 3.13; a CPython point-release tweak must be re-synced.
  Mitigated by the conformance diff catching drift immediately.
- **Marshal `FLAG_REF` correctness.** The object-reference table is a
  known footgun (ordering of `w_ref` registration must match CPython or
  back-references desync). Covered by `test_marshal` identity cases.

## Alternatives

- **Do the full RFC 0007 internal re-encoding now.** Re-encode the VM to
  16-bit `_Py_CODEUNIT`s as the *native* form, so `co_code` is just a
  view of memory. This is the "right" long-term shape and avoids the
  dual representation, but it rewrites the dispatch loop, every inline
  cache, the JIT's bytecode reader, and the observability hooks at once
  — high risk to the green baseline for no additional *compatibility*
  surface beyond what the codec already exposes. We deliberately ship
  the observable surface first (this RFC) and re-encode underneath later
  (RFC 0007), with this RFC's `dis`/`ast` conformance diff as the safety
  net for that change.
- **Hand-write `dis`/`ast` in Rust.** More "native," but re-deriving
  CPython's exact `dis` column layout and `ast.dump` formatting by hand
  is strictly more error-prone than running CPython's own
  `dis.py`/`ast.py` over a CPython-shaped surface. Frozen-Python-over-
  thin-core is the higher-fidelity, lower-LOC choice and matches the
  project's existing pattern.
- **Keep `b"WPY0"` `.pyc` and skip CPython magic.** Rejected: it leaves
  `importlib.util.MAGIC_NUMBER` lying about the format and blocks any
  tool that reads/writes `.pyc` from interoperating. The cache-tag
  (`weavepy-313`) already prevents collisions, so adopting CPython's
  magic costs nothing and buys interop.
- **Skip `co_positions()` (line-only).** Cheaper, but PEP 657 carets are
  a visible 3.13 behavior `test_traceback`/`test_exceptions` assert on,
  and threading columns now is what makes the location table worth
  building once.

## Prior art

- **CPython 3.11–3.13** — `_PyCode_New`, the adaptive `co_code_adaptive`
  vs canonical `co_code` split (the reason a non-adaptive decoder
  suffices for `.pyc`), the PEP 626/657 location tables, and `dis.py`'s
  `show_caches`/`adaptive` rendering. We mirror the artifacts directly.
- **PyPy** — exposes a CPython-compatible `dis`/`marshal`/`.pyc` surface
  over a completely different internal bytecode; precedent for the
  "compat surface ≠ internal form" layering this RFC adopts.
- **RustPython** — has `dis`, `marshal`, and an `_ast`/`ast` module over
  its own code objects; a useful reference for the `_ast` node-class
  bridge and the marshal `TYPE_CODE` shape in Rust.
- **GraalPy / Jython** — both surface `ast` and (Graal) CPython-shaped
  bytecode introspection despite non-CPython runtimes; confirms tooling
  compatibility is achievable independent of execution strategy.

## Unresolved questions

- **Exact 3.13 magic vs patch level.** CPython has bumped `MAGIC_NUMBER`
  within the 3.x series historically. We pin to the harness's tracked
  CPython (3.13, `3571`); do we hard-error or warn on a `.pyc` whose
  magic is a *different* 3.13 patch's? (Lean: warn + recompile, as
  CPython does on mismatch.)
- **`stack_effect` source of truth.** Compute in Rust (`_opcode`) and
  let frozen `dis.py` call it, or vendor CPython's table? (Lean: Rust,
  generated from the Part A mapping, to avoid two tables drifting.)
- **`compile()` AST round-trip depth.** How faithfully must
  `compile(ast.parse(src)) == compile(src)`? Identical bytecode is the
  goal; the first cut targets identical *behavior* + identical `dis`,
  with byte-identity tracked as a conformance metric.
- **Column units.** CPython `col_offset` is UTF-8 *byte* offsets in some
  paths and code points in others across versions; we pin to 3.13's
  documented semantics and grade against `co_positions()`.

## Future work

- **RFC 0007 — internal 16-bit re-encoding.** Make the wire form the
  native form, retiring the dual representation; graded by this RFC's
  `dis`/`ast` conformance diff.
- **Full foreign-`.pyc` execution.** Complete the canonical-opcode
  decoder so a `.pyc` produced by stock CPython 3.13 runs directly under
  WeavePy.
- **`compile()` byte-identity.** Drive `compile(src)` bytecode to be
  byte-for-byte identical to CPython's, not just behavior- and
  `dis`-identical (depends on RFC 0007).
- **`-X showrefcount` / `sys._getframe` code introspection** parity for
  debuggers that walk `f_code`.
- **`dis --specialized`** rendering of the *adaptive* bytecode (requires
  exposing `co_code_adaptive` snapshots; ties into RFC 0021/0032).
- **`ast.PyCF_*` flags** (`PyCF_TYPE_COMMENTS`, `PyCF_ALLOW_TOP_LEVEL_AWAIT`)
  full coverage.

## Implementation status (post-merge)

Legend: ✅ landed · 🟡 landed with a scoped follow-up (see notes).

| area | status | notes |
|------|--------|-------|
| `cpython_code` codec (encode) | ✅ | `cpython_code.rs`: opcode map + `EXTENDED_ARG` + `CACHE` insertion + relative-jump fixpoint; CPython-validated unit tests |
| `cpython_code` codec (decode) | 🟡 | total for WeavePy-emitted streams; full foreign-`.pyc` opcode decode is deferred (Future work) |
| stack-depth pass (`co_stacksize`) | ✅ | abstract max-depth walk + balance check |
| `co_linetable` (PEP 626) | ✅ | span plumbing + varint encoder/decoder; round-trips via `co_lines()` |
| `co_positions()` (PEP 657) | ✅ | four-field `(lineno, end_lineno, col, end_col)` positions |
| `co_exceptiontable` | ✅ | varint range table encode/decode |
| `code` object `co_*` surface | 🟡 | attributes + `co_lines()`/`co_positions()` + `_varname_from_oparg()`; `replace()` overrides directly-stored fields and accepts/ignores derived ones (`co_code`, `co_stacksize`, `co_qualname`, …) |
| `marshal` `TYPE_CODE` + `FLAG_REF` | ✅ | CPython field order + shared-ref table + exact 15-bit bigint; byte-cross-validated against CPython 3.13 |
| `.pyc` CPython magic + real write | ✅ | adopts 3.13 magic `b"\xf3\r\r\n"` + PEP 552 timestamp header; distinct `weavepy-3.13` cache tag avoids collisions |
| `opcode` (frozen + `_opcode`) | ✅ | self-contained CPython 3.13 tables + `stack_effect` |
| `dis` (frozen) | ✅ | CPython-faithful text over the new code surface; honours `file=` and returns strings |
| `ast` (`_ast` core + frozen) | 🟡 | `parse`/`dump`/`walk`/visitors/`literal_eval`/location helpers; `compile(tree)` round-trip deferred (Future work) |
| `symtable` (`_symtable` core + frozen) | ✅ | two-phase native scope analyzer; CPython-3.13-identical classification |
| conformance `ast`/`dis` phases live | ✅ | phases run and emit a graded diff (non-blocking job; grades the raw Rust IR, not the frozen drop-ins) |
| regrtest + fixtures | ✅ | 6 bundled tests (`test_code_object_surface`, `test_dis_dropin`, `test_marshal_roundtrip`, `test_pyc_roundtrip`, `test_ast_dropin`, `test_symtable_dropin`) — pass on both WeavePy and CPython 3.13 |

### Known divergences (tracked, intentional)

- **`co_consts[0]` is not unconditionally `None`.** CPython reserves
  slot 0 of every `co_consts` for `None`; WeavePy only includes constants
  the function references. This is a compiler-internal indexing detail
  with no observable effect on the drop-in modules, deferred rather than
  forced.
- **`code.replace()` is field-level, not a recompile.** It rewrites
  directly-stored fields; derived fields (`co_code`, `co_stacksize`,
  `co_qualname`, `co_flags`, …) are accepted for API compatibility but
  recomputed/ignored rather than honoured verbatim. Full re-derivation
  ties into RFC 0007.
- **Conformance `ast`/`dis` rates remain low and non-blocking.** Those
  phases grade WeavePy's *raw* lexer/parser/compiler IR against CPython,
  not the CPython-faithful frozen `ast`/`dis` modules this RFC ships; the
  drop-in fidelity is instead gated by the six bundled regrtests above.
