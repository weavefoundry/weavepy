# RFC 0052: Conformance wave 7 — compiler front-end fidelity: real `compile()`, pegen-exact syntax errors, `tokenize`/`symtable`, and patchable builtins

- **Status**: Accepted
- **Authors**: WeavePy authors
- **Created**: 2026-07-13
- **Tracking issue**: TBD
- **Builds on**: RFC 0051 (wave 6 — verbatim `typing` + core-language
  burn-down), RFC 0049 (measured whole-suite baseline protocol),
  RFC 0033 (CPython-faithful code objects, `_ast`/`dis`/`symtable`
  cores), RFC 0005 (pegen-exact f-string errors — the pattern this
  wave generalizes).

## Summary

Wave 6 left the sweep at 254 of 427 vendored CPython 3.13 `Lib/test/`
labels passing. Reading the remaining red rows, the largest coherent
cluster is the **compiler front end**: `compile()` accepts no keyword
arguments and cannot compile an AST object (it re-parses stashed
source text), `PyCF_*`/`CO_FUTURE_*` flags don't exist, `optimize`
levels are ignored, a dozen suites fail on non-pegen `SyntaxError`
messages (`test_syntax`, `test_eof`, `test_global`, `test_flufl`,
`test_future_stmt`, …), `tokenize` still emits pre-PEP-701 f-string
tokens, and `symtable` lacks the PEP 695/annotation block types that
3.13's tests probe first.

Wave 7 makes the front end *real*:

1. **`compile()` grows its full CPython signature** — `flags`,
   `dont_inherit`, `optimize`, keyword acceptance, `PyCF_ONLY_AST`,
   `PyCF_ALLOW_TOP_LEVEL_AWAIT`, `PyCF_DONT_IMPLY_DEDENT`,
   `PyCF_ALLOW_INCOMPLETE_INPUT`, `PyCF_TYPE_COMMENTS` (accepted),
   and `CO_FUTURE_*` threading — over a new **Python-AST → Rust-AST
   converter** so `compile(tree, …)` compiles the tree the caller
   built (pytest assertion rewriting's exact shape), not a stashed
   copy of the original text.
2. **Syntax errors go pegen-exact beyond f-strings** — the
   unterminated-string family ("detected at line N"), `unexpected EOF
   while parsing`, `global`/`nonlocal` declaration-ordering errors
   from a symtable-fidelity pass, `barry_as_FLUFL` grammar switching,
   and future-feature diagnostics.
3. **`tokenize` becomes 3.13-faithful** — a native `_tokenize` core
   over `weavepy-lexer` emitting PEP 701 `FSTRING_START` /
   `FSTRING_MIDDLE` / `FSTRING_END` triples, exact token types, and
   the `_generate_tokens_from_c_tokenizer` internal the test suite
   drives.
4. **`symtable` completes** — PEP 695 `type alias` / `type
   parameters` / `TypeVar` bound blocks and annotation scopes, plus
   `filename`/`compile_type` wiring.
5. **Builtins become patchable** — the interpreter's private builtins
   dict and `sys.modules['builtins'].__dict__` unify into one shared
   dict, `LOAD_GLOBAL` resolves builtins through the frame's
   `__builtins__`, and the inline caches gain version guards, so
   `unittest.mock.patch('builtins.open')` behaves like CPython.

As with every wave since RFC 0036, the deliverable is *measured*: the
full sweep is re-run, `tests/regrtest/expectations.toml` is rewritten
from evidence, and every remaining red carries an actionable
first-failure reason.

## Motivation

1. **`compile()`-from-AST is the drop-in gate for test tooling.**
   pytest's assertion rewriting is `ast.parse` → `NodeTransformer` →
   `compile(tree, path, "exec", dont_inherit=True)`. Today that
   compiles the *original* source (the stashed-text hack), silently
   dropping the rewrite — assertions "pass" without introspection.
   coverage.py, hypothesis, numba, attrs, and the standard library's
   own `codeop`/`doctest` also feed flags or trees into `compile()`.
   The measured `test_compile` row fails ~60 of ~190 tests on exactly
   this surface.
2. **Syntax-error fidelity is cheap conformance with a proven
   pattern.** RFC 0005 already made f-string sub-parse errors
   pegen-exact; the same architecture extends to the remaining
   message families. Six labels fail *first* on message shape alone
   (`test_eof`, `test_global`, `test_flufl`, `test_future_stmt`,
   `test_syntax`, `test_source_encoding`), and `test_grammar`'s
   measured reason names top-level await.
3. **`tokenize` is load-bearing for the tooling ecosystem.** inspect,
   doctest, IPython, coverage, black, and 2to3-era tools consume it.
   The frozen pre-PEP-701 port mis-tokenizes every f-string in
   3.12+ style code; `test_tokenize` (3,243 lines) is skipped with a
   stale reason.
4. **Patchable builtins close a semantic hole, not a test hack.**
   CPython resolves global-scope misses through the frame's
   `f_builtins`, which is `builtins.__dict__` — one namespace, user
   mutable. WeavePy's two-dict scheme diverges the moment anything
   writes through a path the mirror doesn't cover (`mock.patch`,
   `dict.__setitem__`, `exec` with custom `__builtins__`). It is the
   measured blocker on `test_argparse` and a residual in the mock
   cluster (`test_unittest`, `test_ensurepip`, `test_mimetypes` had
   to work around it).
5. **Cost of inaction.** The README's drop-in claim is graded by the
   sweep *and* by "clone a project, run its pytest suite". Both are
   currently capped by the front end: the interpreter runs the code
   but cannot ingest the ecosystem's compile-time metaprogramming.

## CPython reference

- `Python/pythonrun.c`, `Python/compile.c`, `Python/ast.c` —
  `compile()` semantics: flag validation (`PyCF_MASK`,
  `PyCF_MASK_OBSOLETE`), `dont_inherit`, `optimize` (-1/0/1/2),
  AST-object input via `PyAST_obj2mod` (mode/node-type agreement,
  recursive field validation, exact `TypeError`/`ValueError` shapes).
- `Include/cpython/compile.h` + `Lib/ast.py` — the `PyCF_*` constant
  values (`PyCF_ONLY_AST` 0x400, `PyCF_TYPE_COMMENTS` 0x1000,
  `PyCF_ALLOW_TOP_LEVEL_AWAIT` 0x2000, `PyCF_OPTIMIZED_AST`
  0x400|0x8000, `PyCF_DONT_IMPLY_DEDENT` 0x200,
  `PyCF_ALLOW_INCOMPLETE_INPUT` 0x4000) and `Lib/__future__.py` —
  `CO_FUTURE_*` bits.
- `Parser/pegen_errors.c`, `Parser/tokenizer/helpers.c` — the
  unterminated-string / EOF message family
  (`"unterminated string literal (detected at line %d)"`,
  `"unterminated triple-quoted string literal (detected at line %d)"`,
  `"unexpected EOF while parsing"`) and error-location rules.
- `Python/symtable.c` — `"name '%s' is assigned to before global
  declaration"`, `"name '%s' is used prior to global declaration"`
  (and the `nonlocal` twins), block types (`TypeAliasBlock`,
  `TypeParametersBlock`, `TypeVariableBlock`, `AnnotationBlock`),
  and `Modules/symtablemodule.c` for the `_symtable` surface.
- `Parser/tokenizer/*.c` + `Lib/tokenize.py` (3.13) — PEP 701
  f-string tokens, `TokenizerIter`, `detect_encoding`,
  `_generate_tokens_from_c_tokenizer`.
- `Grammar/python.gram` — `barry_as_FLUFL` (`'<>' { … barry_as_flufl
  … }`) and `invalid_*` rules for message text.
- `Python/ceval.c` `_PyEval_GetBuiltin` / `frameobject.c`
  `f_builtins` — single-namespace builtins resolution;
  `Python/specialize.c` `LOAD_GLOBAL` version guards.
- Acceptance tests: `Lib/test/test_compile.py`, `test_syntax.py`,
  `test_eof.py`, `test_global.py`, `test_flufl.py`,
  `test_future_stmt/`, `test_source_encoding.py`, `test_tokenize.py`,
  `test_symtable.py`, `test_type_comments.py`, `test_grammar.py`,
  `test_codeop.py`, `test_code_module.py`, `test_unparse.py`,
  `test_argparse.py`, and the mock-cluster residuals.

## Detailed design

### WS1 — `compile()` full surface + AST-object lowering

**Signature.** `do_compile_call` gains CPython's exact signature
(`source, filename, mode, flags=0, dont_inherit=False, optimize=-1`,
all usable as keywords via a `call_kw` binding), with CPython's
validation order and error shapes: unknown flag bits →
`ValueError("compile(): unrecognised flags")`, `optimize` outside
{-1, 0, 1, 2} → `ValueError("compile(): invalid optimize value")`,
oversized ints → `OverflowError`, `filename` accepting `str`, `bytes`,
and `os.PathLike` with embedded-NUL rejection.

**Constants.** A new frozen-`ast`-visible constant surface:
`ast.PyCF_ONLY_AST`, `PyCF_TYPE_COMMENTS`, `PyCF_ALLOW_TOP_LEVEL_AWAIT`,
`PyCF_OPTIMIZED_AST` on `ast` and `_ast`; `PyCF_DONT_IMPLY_DEDENT` and
`PyCF_ALLOW_INCOMPLETE_INPUT` honored where `codeop` already defines
them. `__future__.CO_FUTURE_*` values become real: the compiler
records active futures into `co_flags`, and `dont_inherit=False`
inherits the *calling frame's* future bits like CPython.

**AST-object input.** The stashed-source hack is replaced by a real
converter in `ast_mod.rs`: `obj2ast` walks a Python AST instance
(any object exposing `_fields`, matching `PyAST_obj2mod`'s duck
typing) and rebuilds the `weavepy_parser::ast` tree, validating node
types, field arity, position attributes (with CPython's
missing-`lineno` error text), and mode/root-node agreement
(`exec`→`Module`, `eval`→`Expression`, `single`→`Interactive`).
`compile(tree, …)` then flows through the normal compiler. The
`_weavepy_source` stash is deleted. `PyCF_ONLY_AST` returns the parse
tree (built by the existing Rust→Python builder) without compiling;
`ast.parse` becomes literally `compile(source, filename, mode,
PyCF_ONLY_AST | extra_flags)`.

**`optimize` levels.** Threaded through `Compiler::new`:
level ≥ 1 folds `__debug__` to `False` and strips `assert`
statements; level 2 additionally drops docstrings (module, class,
function — `co_consts[0]` shape matching CPython). The CLI's
`-O`/`-OO` set the interpreter default so `sys.flags.optimize`,
`__debug__`, and bare `compile()` agree.

**Top-level await.** `PyCF_ALLOW_TOP_LEVEL_AWAIT` compiles module
code with `CO_COROUTINE`, permitting `await`/`async for`/`async with`
at top level (the `asyncio` REPL contract). Without the flag the
existing `'await' outside function` error stands.

### WS2 — pegen-exact syntax errors beyond f-strings

Following the RFC 0005 layering (lexer emits structured errors,
parser maps them to CPython message families, VM shapes the final
`SyntaxError`):

- **Unterminated strings.** `LexError::UnterminatedString` splits
  into single-line and triple-quoted variants carrying the *detection
  line*; messages become `unterminated string literal (detected at
  line N)` / `unterminated triple-quoted string literal (detected at
  line N)` with the opening-quote offset and stripped-`\n` `.text`,
  matching `test_eof` byte-for-byte (including the latin-1-cookie and
  BOM re-lining cases).
- **EOF continuation.** A trailing `\` before EOF raises
  `unexpected EOF while parsing` with CPython's offset (end of line,
  `.text` keeping the backslash + `\n`).
- **Declaration ordering.** The `validate.rs` symtable pass tracks
  per-scope first-use/first-assignment/first-annotation positions and
  raises `name 'x' is assigned to before global declaration`,
  `… is used prior to global declaration`, `… is parameter and
  global`, and the `nonlocal` twins, at the *directive's* position
  (`test_global` asserts lineno/offset of the `global` statement).
- **FLUFL.** The lexer learns `<>` as a token; the parser accepts it
  as `!=` only when `barry_as_FLUFL` is active (from a `__future__`
  import seen earlier in the token stream, or `CO_FUTURE_BARRY_AS_BDFL`
  in `compile()` flags) and then *rejects* `!=` with `with Barry as
  BDFL, use '<>' instead of '!='`; inactive `<>` stays bare
  `invalid syntax` at CPython's offset.
- **Future features.** `test_future_stmt`'s message set: `future
  feature X is not defined`, placement errors, and `not a chance`
  keep their current text but gain CPython's positions; new:
  `__future__` imports set `CO_FUTURE_*` bits observable on
  `co_flags` (WS1).
- **`test_syntax` burn-down.** The suite is doctest-driven message
  comparison; work the measured diffs (the `'invalid syntax'`
  substring family, `cannot assign to …` positions, `expected ':'`
  hints) until the residual is enumerable, recording what remains.

### WS3 — 3.13-faithful `tokenize`

A native `_tokenize` module (new `tokenize_mod.rs`) exposes
`TokenizerIter` over `weavepy-lexer`:

- Emits CPython 3.13 token streams: exact-type mapping
  (`OP` exact types via `EXACT_TOKEN_TYPES`), `NL` vs `NEWLINE`,
  `INDENT`/`DEDENT`, `COMMENT`, and PEP 701 `FSTRING_START` /
  `FSTRING_MIDDLE` / `FSTRING_END` with CPython's interior-token
  re-tokenization of replacement fields. The lexer already scans
  f-string fields structurally (RFC 0005); the iterator re-projects
  those spans as token triples rather than one `STRING`.
- `(line, col)` positions computed the way CPython reports them
  (character columns, `''`-line synthetic tokens at EOF).
- Frozen `tokenize.py` is replaced by CPython 3.13's file, verbatim
  per the adoption policy, over the native core
  (`_tokenize.TokenizerIter`), keeping `detect_encoding` behavior and
  the `_generate_tokens_from_c_tokenizer` internal the tests import.
- `test_tokenize`'s skip row is retired; the suite is measured.

### WS4 — `symtable` completion

`symtable_mod.rs` grows the 3.13 block model:

- PEP 695 constructs produce their dedicated blocks: `type X = …` →
  `TypeAliasBlock`, generic params on `def`/`class` →
  `TypeParametersBlock`, TypeVar bounds/defaults →
  `TypeVariableBlock`; annotations under `from __future__ import
  annotations` or in stubs produce `AnnotationBlock` where CPython
  does.
- `filename` threads into parse errors; `compile_type` selects
  exec/eval/single parsing like `_symtable.symtable`.
- Wrapper `symtable.py` re-syncs with 3.13 (`Class.get_methods()`
  deprecation shape, `SymbolTableType` enum values).

### WS5 — patchable builtins

Per the RFC 0024/0031 engine-work conventions:

- **One namespace.** `sys.modules['builtins'].dict` *is* the
  interpreter's builtins `Rc` — the frozen `builtins.py` copy loop
  and the `store_attr` mirroring are deleted. `PyFrame.builtins` and
  `function.__builtins__` observe the same dict.
- **Frame-scoped resolution.** `LOAD_GLOBAL`'s slow path and the
  specializer resolve builtins via `globals['__builtins__']` (module
  or dict, falling back to the interpreter dict), matching
  `_PyEval_GetBuiltin`, so `exec(code, {'__builtins__': {...}})`
  behaves.
- **Cache correctness.** The `LoadGlobalModule`/`LoadGlobalBuiltin`
  inline caches gain the dict version guards their `bytecode.rs`
  comment already promises (a per-dict `Cell<u64>` bumped on any
  structural change or builtins write), so a patched `open` deopts
  the cache instead of serving the stale slot. Call-site intercepts
  in `dispatch_call` only trigger when the loaded object still *is*
  the original builtin.

### WS6 — re-measure and re-baseline

Per the RFC 0049 protocol: two full sweeps
(`weavepy-conformance regrtest --all-cpython --mode subprocess
--jobs 8`), cross-checked; `expectations.toml` rewritten so every
row is measured; bundled fixtures stay green; new bundled regrtests
land for the novel surfaces (compile-from-AST round-trips incl. a
pytest-shaped assert rewrite, `PyCF_ONLY_AST`, optimize levels,
FLUFL, unterminated-string messages, PEP 701 tokenize streams,
PEP 695 symtable blocks, builtins patching through `mock.patch` and
raw dict mutation).

### Acceptance criteria

1. `compile(ast_tree, file, mode)` compiles the *given* tree:
   a mutated-AST fixture (pytest-style assert rewrite) observably
   executes the rewritten code; `compile(src, f, m, PyCF_ONLY_AST)`
   returns an `ast.Module`.
2. `cpython/Lib/test/test_eof.py`, `test_global.py`, `test_flufl.py`,
   and `test_future_stmt` flip to measured `pass`; `test_compile.py`
   and `test_syntax.py` flip to `pass` or to measured rows whose
   residuals are enumerated and small (they are the two largest
   suites in the cluster).
3. `test_tokenize.py` is unskipped and measured; PEP 701 f-string
   token triples match CPython on the bundled fixtures.
4. `test_symtable.py` flips to measured `pass`.
5. `mock.patch('builtins.open')` affects `open()` observed via
   `LOAD_GLOBAL` in freshly compiled and *already-specialized* code;
   `test_argparse.py`'s builtins-patching error clears (measured).
6. At least 10 net labels flip red→green on the full sweep versus the
   wave-6 baseline.
7. `cargo fmt` / `clippy -D warnings` / `cargo test --workspace` /
   `regrtest --check` all green.

## Drawbacks

- **The AST converter is a big, fiddly surface** (~100 node types ×
  field validation with exact error shapes). Mitigated by driving it
  from the same node table the Rust→Python builder already encodes,
  and by `test_compile`/`test_ast` grading both directions.
- **Unifying the builtins dict touches interpreter startup order.**
  The frozen `builtins` module must exist before arbitrary imports;
  regressions here break everything at once. Mitigated by keeping the
  interpreter's dict as the single source and pointing the module at
  it (not the reverse), so pre-module-load lookups are unchanged.
- **Dict version guards add a branch to hot lookups.** The guard is a
  `u64` load+compare on the specialized path — the same cost CPython
  pays; the perf RFCs' benchmarks gate regressions.
- **PEP 701 tokenize re-projection duplicates f-string structure
  knowledge** between the scanner and the token iterator. Accepted:
  the scanner already owns field spans; the iterator is a projection,
  not a second tokenizer.

## Alternatives

- **Keep the stashed-source hack and special-case pytest** (teach the
  rewriter to hand back source): rejected — every AST-mutating tool
  would need its own hack, and CPython's `TypeError`/`ValueError`
  validation surface would stay unimplementable.
- **Unparse the Python AST to text and re-parse** instead of a real
  converter: rejected — loses exact positions (PEP 657 columns in
  tracebacks would lie about rewritten code), can't represent
  synthetic trees with deliberate positions, and diverges from
  CPython's validation error shapes.
- **Port CPython's C tokenizer wholesale** for WS3: rejected —
  `weavepy-lexer` is already conformance-graded against the CPython
  oracle; a projection layer is ~10× smaller and keeps one lexer.
- **Extend `store_attr` mirroring instead of unifying the dicts**:
  rejected — `dict.__setitem__`, `dict.update`, and mock internals
  bypass attribute stores; the two-dict scheme is unfixable by
  patching sync points (the current bug is the proof).

## Prior art

- **CPython** is the spec throughout; `PyAST_obj2mod` +
  `ast_for_*` validation define the converter's contract, and 3.12's
  `tokenize`-over-C-iterator rewrite (gh-102856) defines WS3's shape.
- **PyPy** compiles from `ast` objects natively (its compiler ingests
  the app-level AST); pytest assertion rewriting has worked on PyPy
  for a decade — evidence the converter approach, not the unparse
  shortcut, is the durable one.
- **RustPython** exposes `compile(tree, …)` via an AST-to-bytecode
  path over rustpython-ast and hit the same mode/node-agreement
  error-shape details; their issue tracker documents the long tail
  this RFC's validation matrix covers up front.
- **RFC 0005 (f-strings)** proved the layered pegen-message pattern
  inside this codebase; WS2 is its generalization.

## Unresolved questions

- Whether `test_compile`'s optimizer-shape classes
  (`TestStackSizeStability`, `TestInstructionSequence`, peephole
  assertions) can pass without adopting CPython's exact block-layout
  optimizer, or land as an enumerated residual. Acceptance
  criterion 2 allows either.
- Whether `PyCF_TYPE_COMMENTS` gets real `# type:` parsing this wave
  or validated-but-inert acceptance with `test_type_comments`
  measured red on the parsing arc (the flag plumbing lands either
  way).
- How far the `test_syntax` doctest matrix (2,723 lines of message
  comparisons) converges inside the wave budget.

## Future work

- A CPython-shaped control-flow-graph optimizer pass (would retire
  the `test_compile` optimizer residual and most of
  `test_compiler_codegen`).
- `PyCF_OPTIMIZED_AST` constant folding on the returned tree.
- Wave 8 candidates unblocked here: `doctest`/`unittest` residuals
  (need patchable builtins + compile flags), `codeop`/REPL parity
  (needs `PyCF_ALLOW_INCOMPLETE_INPUT`), coverage.py support
  (needs `tokenize` + trace fidelity).
