# RFC 0035: A faithful `re`/`_sre` engine — porting CPython's secret-labs matcher, with the Unicode and `%`-formatting fidelity it demands

- **Status**: Accepted
- **Authors**: WeavePy authors
- **Created**: 2026-05-31
- **Tracking issue**: TBD
- **Builds on**: RFC 0012 (modules/imports + frozen stdlib), RFC 0015
  (object-model completion), RFC 0020 (real-Python frozen stdlib),
  RFC 0023 (drop-in parity), RFC 0030 (pure-Python drop-in), RFC 0034
  (the CPython test suite as a live harness — `test_re.py` is exactly
  the kind of file it was built to run)

## Summary

WeavePy's `re` module was, until this RFC, a **translation layer**: a
native Rust shim (`stdlib/re.rs`) that parsed a subset of Python's
regex syntax and forwarded it to the Rust [`regex`] and [`fancy_regex`]
crates. That bought us working `re.match`/`search`/`findall` for the
common cases quickly, but it was a parallel implementation of a
notoriously corner-heavy language. Anywhere CPython's behaviour is
defined by *the secret-labs engine itself* — backtracking order, the
exact group-reset semantics on alternation, zero-width-match
bookkeeping, `\b` at the bytes/str boundary, the precise text of a
`re.error`, `Pattern`/`Match` repr and attribute surface, the
`re.Scanner` undocumented-but-tested API — a re-implementation can only
approximate, and the approximations are exactly what `Lib/test/test_re.py`
exists to catch.

This RFC replaces the shim with **CPython's own engine**:

1. A **native `_sre` core** (`crates/weavepy-vm/src/stdlib/sre_mod.rs`)
   — a faithful port of `Modules/_sre/sre_lib.h`'s backtracking VM
   (`SRE(match)`, `SRE(search)`, `SRE(count)`, `SRE(charset)`), the
   case-folding/character-classification primitives, and the module
   surface real code touches (`compile`, the compiled-pattern `exec`
   trampoline, `ascii_tolower`/`unicode_tolower`,
   `ascii_iscased`/`unicode_iscased`, `getlower`, `getcodesize`, plus
   `MAGIC`/`CODESIZE`). Compiled programs live in a thread-local
   registry keyed by an integer handle, so the Rust core stays free of
   Python-object lifetime concerns.
2. The **frozen Python `re` package**, ported from CPython 3.13:
   `re/__init__.py`, `re._constants`, `re._casefix`, `re._parser`,
   `re._compiler`, all essentially verbatim, plus a small
   `re._engine` that builds the `Pattern`/`Match` classes on top of
   the native core (CPython builds those in C; we build them in frozen
   Python over a minimal native primitive — see *Detailed design §3*).
   The pre-3.11 deprecated aliases `sre_constants`/`sre_parse`/
   `sre_compile` are shipped as re-export shims.
3. The **Unicode, `str`/`bytes`, and `%`-formatting fidelity** that the
   real `re` parser/compiler turned out to depend on — and which the
   shim had hidden. Porting CPython's own `_parser.py` surfaced a tail
   of interpreter gaps (`int` subclassing, slice deletion/assignment,
   the legacy `__getitem__` iteration protocol, faithful `repr()`
   printability, `str(bytes, encoding)`, `\U` escapes, `%`-format
   dunder dispatch). Each is a general correctness fix that happened to
   be *forced into the light* by running CPython's code unmodified.

Diff shape: **~6K lines added** — the `_sre` Rust core (~1.6K), the five
frozen `re` submodules + three alias shims (~3K, mostly *CPython's own
Python* carried verbatim), the interpreter/object-model fixes (~1K
across the VM/compiler/parser), `tests/regrtest/test_re.py`, the
module-registry rewiring, and this RFC — against ~1.1K deleted with the
old `stdlib/re.rs` shim (and its VM-level `re.sub`-callable hook,
`do_re_sub_callable`), for a net diff of **~5K LOC**.

That the *fidelity* upgrade is also a *smaller* footprint is the whole
argument: a faithful port reuses CPython's ~3K-line Python parser/
compiler unchanged and reimplements only the ~1.6K-line C matcher,
where a from-scratch shim would have to grow toward the full corner-case
surface line by line and still never reach parity. Compactness here is
evidence the strategy is right, not that the scope is small.

Mission alignment: `re` is one of the most-imported modules in the
stdlib, and `test_re.py` is one of CPython's largest single-module test
files. Running *CPython's engine* rather than an emulation of it is the
difference between "regex mostly works" and "regex is CPython."

## Motivation

The shim was a liability for three compounding reasons:

- **It was a second implementation of a hard language.** Python's regex
  dialect is not PCRE and is not the Rust `regex` crate's dialect.
  Conditional groups `(?(id)yes|no)`, the exact semantics of
  `\b`/`\B`/`\A`/`\Z`, possessive quantifiers and atomic groups
  `(?>...)`, the group-state rollback on a failed alternation branch,
  the rule that an empty match adjacent to a previous match is skipped
  in `findall`/`finditer`/`sub` (`must_advance`), and the textual
  content of every `re.error` ("nothing to repeat", "missing ),
  unterminated subpattern", "redefinition of group name") are all
  defined operationally by the secret-labs engine. `fancy_regex` makes
  *different* choices, so any program that depends on Python's choices
  silently diverged.
- **It could never run `test_re.py`.** RFC 0034 made CPython's own
  `Lib/test/test_re.py` runnable in principle; the shim made it
  unpassable in practice, because the test asserts on engine internals
  (`sre_compile` output sizes, `Pattern.__repr__`, `re.error` line/col,
  `Match.regs`, the `_sre.MAGIC`/`CODESIZE` contract that
  `re._compiler` checks at import).
- **It diverged from the frozen-stdlib strategy.** RFC 0020's thesis is
  "ship real CPython Python where we can." `re` is *the* poster child:
  CPython's `re` is ~90% Python (`_parser`/`_compiler`/`__init__`) over
  a ~10% C core (`_sre`). Porting the Python verbatim and reimplementing
  only the C core is both less code and dramatically more faithful than
  a from-scratch shim.

The cost of inaction was an open-ended tail of "regex behaves subtly
differently" bugs — the worst kind, because they are silent.

## CPython reference

We track **CPython 3.13**. Specific sources ported or matched:

- **The C core**: `Modules/_sre/sre.c`, `Modules/_sre/sre_lib.h`,
  `Modules/_sre/sre_constants.h`, `Modules/_sre/sre.h`. The matcher
  port mirrors the `SRE(match)` opcode dispatch loop, `SRE(search)`'s
  prefix/charset fast-paths, `SRE(count)` for `REPEAT_ONE`/
  `MIN_REPEAT_ONE`, and `SRE(charset)`/`SRE(charset_loc_ignore)`. The
  `_sre.MAGIC` constant (`20230612`) and `CODESIZE` (`4`) are the
  contract `re._compiler` checks at import.
- **The Python layer**: `Lib/re/__init__.py`, `Lib/re/_constants.py`,
  `Lib/re/_casefix.py`, `Lib/re/_parser.py`, `Lib/re/_compiler.py`.
- **The deprecated aliases**: `Lib/sre_constants.py`, `Lib/sre_parse.py`,
  `Lib/sre_compile.py` (each a thin "moved to `re._*`" shim since 3.11).
- **The Unicode/format surface** the engine leans on: the `str`/`bytes`
  data model (the language reference §3), `str.isprintable`/`repr`
  (CPython `Objects/unicodeobject.c`'s `unicode_repr` and
  `Py_UNICODE_ISPRINTABLE`), printf-style `%` formatting
  (`PyUnicode_Format`), and the `str(object, encoding, errors)`
  constructor form.
- **The acceptance test**: `Lib/test/test_re.py` (the parts that don't
  require `_sre` C-detail refleak hooks).

As with RFC 0034, anything the engine reaches for that we deliberately
do not model raises the *same* exception CPython would, so an
unsupported corner reads as the correct error, never a wrong answer.

## Detailed design

### 1 — the native `_sre` core (`stdlib/sre_mod.rs`)

The native module exposes the minimal surface `re._compiler` and
`re._engine` import:

| symbol | role |
|---|---|
| `MAGIC` = `20230612` | version stamp `re._compiler` asserts against `_constants.MAGIC` |
| `CODESIZE` = `4` | word size of the compiled program (`sizeof(SRE_CODE)`) |
| `compile(pattern, flags, code, groups, groupindex, indexgroup)` | intern a compiled program into the thread-local registry; returns an integer handle |
| `exec(handle, string, pos, endpos, …)` | run `SRE(search)`/`SRE(match)` and return the match groups (or `None`) |
| `ascii_tolower` / `unicode_tolower` | case-fold a single code point |
| `ascii_iscased` / `unicode_iscased` | "is this code point case-sensitive?" (drives `IGNORECASE`) |
| `getlower(ch, flags)` | the flag-aware lowercase used by `LITERAL_IGNORE` |
| `getcodesize()` | `CODESIZE`, as a function (the historical API) |

**The matcher.** `SRE(match)` is a recursive backtracking interpreter
over the `u32` program emitted by `re._compiler`. The port keeps the
opcode numbering from `_constants.OPCODES` (so the *Python* compiler and
the *Rust* matcher agree by construction) and implements the full set
real patterns reach: `LITERAL`/`NOT_LITERAL`/`LITERAL_IGNORE`/
`LITERAL_UNI_IGNORE`/`LITERAL_LOC_IGNORE`, `ANY`/`ANY_ALL`, `IN`/
`IN_IGNORE`/`IN_UNI_IGNORE`/`IN_LOC_IGNORE` (delegating to
`SRE(charset)`), `BRANCH`, `REPEAT`/`MAX_UNTIL`/`MIN_UNTIL`,
`REPEAT_ONE`/`MIN_REPEAT_ONE` (with the `SRE(count)` fast path),
`GROUPREF`/`GROUPREF_IGNORE`/`GROUPREF_EXISTS`, `AT` (the
anchors/boundaries), `ASSERT`/`ASSERT_NOT` (look-around),
`MARK`, `JUMP`, `SUCCESS`, `FAILURE`, `INFO`, `ATOMIC_GROUP`/
`POSSESSIVE_REPEAT`/`POSSESSIVE_REPEAT_ONE`.

**Zero-width correctness — the `toplevel`/`must_advance` invariant.**
The single subtlest part of the port. CPython threads a `toplevel` flag
through `SRE(match)` and uses it (with the saved repeat mark) to refuse
a second *empty* iteration of a repeat or branch tail, which is what
keeps `re.findall(r'a*', 'aaa')`, `re.split(r'x*', 'axbxc')`, and
`re.sub(r'', '-', 'abc')` from looping forever or producing the wrong
split. The port reproduces this exactly: `OP_BRANCH`, `OP_REPEAT`,
`OP_MAX_UNTIL`, and `OP_MIN_UNTIL` **inherit** the caller's `toplevel`
into their tail continuations rather than forcing `false` (an early
draft hard-coded `false` and hung on `a{,3}` against `'aaaaa'`). The
`REPEAT_ONE` count loop uses signed arithmetic so a decrement past zero
can't wrap a `usize`.

**Lifetime model.** The Rust matcher never holds a Python object across
a call. `compile` copies the `code: Vec<u32>` and the group metadata
into a thread-local `RegistryEntry` and returns its index as a plain
`int`; `exec` looks the entry up, runs against the subject string/bytes,
and returns owned results. This sidesteps the GC/borrow questions a
native `Pattern` object would raise and keeps the core a pure function
of `(program, subject, pos)`.

### 2 — the frozen Python `re` package

Registered as a frozen package with submodules (the model
`email`/`importlib` already use):

```
re                  (package)   <- Lib/re/__init__.py        (re_init.py)
re._constants       (module)    <- Lib/re/_constants.py      (verbatim)
re._casefix         (module)    <- Lib/re/_casefix.py        (verbatim)
re._parser          (module)    <- Lib/re/_parser.py         (verbatim)
re._compiler        (module)    <- Lib/re/_compiler.py        (≈verbatim)
re._engine          (module)    <- WeavePy-specific glue (Pattern/Match)
sre_constants       (module)    <- re-export shim (deprecated alias)
sre_parse           (module)    <- re-export shim (deprecated alias)
sre_compile         (module)    <- re-export shim (deprecated alias)
```

`_constants`, `_casefix`, and `_parser` are CPython 3.13 **verbatim** —
the whole point is to run CPython's parser, not ours. `_compiler` is
all-but-verbatim: the only adaptations are where it calls into the C
core (it gets `MAGIC`/`CODESIZE` from our native `_sre`, and its
`_bytes_to_codes` helper assembles the `array`-backed program the same
way, which is what surfaced the `int.byteorder`/`array` plumbing fixes
below).

### 3 — `Pattern`/`Match` in frozen Python (`re._engine`)

CPython implements `Pattern` and `Match` as **C types** in `_sre`.
Reproducing those as native Rust objects would mean a second object
type with its own GC integration, attribute table, and repr — a large
surface for little gain. Instead `re._engine` defines `Pattern` and
`Match` as **Python classes** over the native primitive:

- `Pattern` holds the compiled handle, `pattern`/`flags`/`groups`/
  `groupindex`/`groupdict`, and implements `match`/`fullmatch`/`search`/
  `findall`/`finditer`/`split`/`sub`/`subn`/`scanner` by calling
  `_sre.exec` and wrapping results. The scan loop (`_iter`) implements
  the `must_advance` "skip empty match adjacent to previous" rule in
  Python, mirroring `_sre`'s `scanner`/`Pattern.finditer`.
- `Match` exposes `group`/`groups`/`groupdict`/`start`/`end`/`span`/
  `expand`/`__getitem__`/`regs`/`pos`/`endpos`/`lastindex`/
  `lastgroup`/`re`/`string`, with CPython's `repr`.
- **Template expansion** (`re.sub`'s replacement parsing, `\g<name>`
  and `\1` back-references, and the *callable* `repl` path) lives here
  in Python — which is why the old VM-level `do_re_sub_callable`
  interception in `weavepy-vm/src/lib.rs` is **deleted**: a callable
  `repl` is now just a Python call in `_engine`, exactly as in CPython.

This "C core + Python wrapper class" split is the same shape CPython
*almost* has (its wrapper is C only for speed); functionally it is
indistinguishable to user code.

### 4 — interpreter & object-model fixes surfaced by the port

Running CPython's unmodified `_parser.py`/`_compiler.py` is a stress
test of the interpreter. Each gap below was a real CPython-behaviour
bug that the shim had simply never exercised; all are fixed as general
correctness work in `weavepy-vm`/`weavepy-compiler`/`weavepy-parser`:

- **`int` subclassing.** `_parser` and `_constants` use
  `_NamedIntConstant(int)` for opcodes, and `enum.IntFlag`/`IntEnum`
  back the `re` flags. `PyInstance` gained a `native: Option<Object>`
  slot; `object.__new__` initialises it for `int`/`float` subclasses;
  and every arithmetic/identity/hash/ordering/truth path
  (`as_i64`, `as_usize`, `eq_value`, `DictKey::hash`, `is_truthy`,
  `Object::cmp`, `binary_op`, `binary_subscr`, the `int()`/`float()`/
  `bool()` constructors) now unwraps a subclass to its native value.
  `enum.py`'s `IntEnum`/`IntFlag` were re-based on `(int, Enum)` /
  `(int, Flag)` and member creation routed through `int.__new__`.
- **Slice deletion & assignment.** `_parser` does `del subpattern[x]`
  and `subpattern[a:b] = …`. Added `del seq[slice]` for `list`/
  `bytearray`, slice-assignment from an arbitrary iterable RHS
  (via the VM's `collect_iterable`), `range` slicing, and correct
  negative-step (`[::-1]`) handling mirroring `PySlice_AdjustIndices`.
- **Legacy iteration protocol.** Objects with `__getitem__`+`__len__`
  but no `__iter__` are now iterable (call `__getitem__(0,1,2,…)` until
  `IndexError`), which `re`'s `SubPattern` relies on.
- **The ABA call-cache bug.** `MAKE_FUNCTION`'s inline cache could
  mis-specialize when a freed function's `Rc` address was reused
  (classic ABA). `CallPyExact`/`CallPyExactNoFree` now re-validate the
  callee's closure shape and arg count before taking the fast path.
- **`bytes`/`bytearray` methods.** `translate`/`maketrans` implemented;
  `find`/`rfind`/`index`/`count` now honour `start`/`end` (bytes
  patterns go through these).
- **Truthiness dispatch.** A shared `obj_truthy` that dispatches
  `__bool__` then `__len__` for instances, wired into `PopJumpIfFalse`/
  `PopJumpIfTrue`/`UnaryOp(not)`/`bool()` — without it `(?i)`-style
  inline-flag parsing mis-fired.
- **Compiler import binding.** `collect_decls` now records names bound
  by `import`/`from … import` so they can be captured as cellvars
  (a closure in `_compiler` referenced an imported name).

### 5 — Unicode, `str`/`bytes`, and `%`-formatting fidelity

- **Faithful `repr()`.** `str.__repr__` now picks CPython's quote
  (double quotes iff the string has a `'` and no `"`, else single) and
  escapes non-printable code points as `\xNN`/`\uNNNN`/`\UNNNNNNNN`.
  Printability is decided by Unicode general category via the
  `unicode_properties` crate (`Cc`/`Cf`/`Cs`/`Co`/`Cn` and the
  separators are non-printable; `U+0020` is the one printable space),
  matching `Py_UNICODE_ISPRINTABLE`. `str.isprintable` shares the
  helper. (`re.escape` and `Pattern.__repr__` both depend on this.)
- **`str(bytes, encoding[, errors])`.** The two/three-arg `str`
  constructor now decodes via the codec machinery instead of returning
  `repr(b'…')`. `re._parser.Tokenizer` builds itself from
  `str(byte, 'latin-1')`, so without this every *bytes* pattern was
  mis-tokenised.
- **`\U` escapes.** The lexer's string-literal decoder learned the
  eight-hex-digit `\U` form (it already handled `\x`/`\u`), so non-BMP
  literals like `'\U0001f600'` parse correctly.
- **`%`-format dunder dispatch.** `str.__mod__` (`"%s"/"%r" % obj`) now
  dispatches `__str__`/`__repr__` for instances (so `"%s" % some_error`
  prints the message, not `<PatternError object>`), and `%d`/`%i`/`%u`
  unwrap `int` subclasses (so `"%d" % OPCODES.LITERAL` formats the
  value). Implemented by threading a VM-aware `resolve` callback into a
  refactored `percent_format_with`.

### 6 — module rewiring & test

- `stdlib/mod.rs` registers `_sre` as a builtin native module and the
  nine frozen sources above; the old native `re` registration and
  `stdlib/re.rs` are removed.
- `weavepy-vm/src/lib.rs` drops `do_re_sub_callable` (now handled in
  frozen `re._engine`).
- `tests/regrtest/test_re.py` is a new bundled fixture (auto-discovered
  by the RFC 0034 harness) covering literals/quantifiers/groups/
  alternation/flags, look-around, back-references, named groups,
  `split`/`sub`/`subn`/`findall`/`finditer`, bytes patterns, Unicode
  categories, `re.error` text, and the zero-width edge cases that the
  `toplevel` invariant protects. Every expectation was cross-checked
  against the local CPython 3.13 oracle.

## Implementation status (post-merge)

| area | status | notes |
|------|--------|-------|
| native `_sre` matcher (`SRE(match)`/`search`/`count`/`charset`) | ✅ | full opcode set incl. look-around, back-refs, possessive/atomic, conditional groups |
| `_sre` module surface (`compile`/`exec`/case-fold/`MAGIC`/`CODESIZE`) | ✅ | thread-local compiled-program registry keyed by int handle |
| zero-width `toplevel`/`must_advance` invariant | ✅ | `BRANCH`/`REPEAT`/`MAX_UNTIL`/`MIN_UNTIL` inherit `toplevel`; signed `REPEAT_ONE` count |
| frozen `re` package (`__init__`/`_constants`/`_casefix`/`_parser`) | ✅ | CPython 3.13 verbatim |
| frozen `re._compiler` | ✅ | ≈verbatim; targets our native `_sre` `MAGIC`/`CODESIZE` |
| `re._engine` `Pattern`/`Match` + template expansion | ✅ | Python classes over the native core; callable `repl` is plain Python |
| deprecated `sre_constants`/`sre_parse`/`sre_compile` aliases | ✅ | re-export shims |
| removed: native `stdlib/re.rs` + `do_re_sub_callable` VM hook | ✅ | the shim and its VM interception are gone |
| `int`/`float` subclassing (`native` slot; arith/hash/cmp/truth) | ✅ | `enum.IntFlag`/`IntEnum`, `_NamedIntConstant` work |
| slice delete/assign, `range` slicing, negative step | ✅ | `del seq[slice]`, `seq[a:b]=iter`, `r[::-1]` |
| legacy `__getitem__` iteration protocol | ✅ | `__getitem__`+`__len__` without `__iter__` iterates |
| ABA inline-cache hardening (`MAKE_FUNCTION`/`CallPyExact*`) | ✅ | closure-shape + arg-count re-validation |
| `bytes`/`bytearray` `translate`/`maketrans`; `find`-family `start`/`end` | ✅ | bytes patterns rely on these |
| truthiness dispatch (`__bool__`/`__len__`) | ✅ | wired into jumps/`not`/`bool()` |
| faithful `repr()`/`isprintable` (Unicode general category) | ✅ | quote selection + `\xNN`/`\uNNNN`/`\UNNNNNNNN` escaping |
| `str(bytes, encoding[, errors])`; lexer `\U` escapes | ✅ | bytes patterns + non-BMP literals |
| `%`-format `__str__`/`__repr__` dispatch + int-subclass unwrap | ✅ | `"%s" % exc`, `"%d" % OPCODE` |
| bundled `tests/regrtest/test_re.py` | ✅ | passes under WeavePy and CPython 3.13 |

## Drawbacks

- **The matcher is recursive, like CPython's.** `SRE(match)` recurses
  per opcode group; pathological patterns can hit the native stack
  before Python's `sys.setrecursionlimit`. CPython has the same shape
  (and the same class of failure); a future iterative/explicit-stack
  rewrite is possible but out of scope.
- **Two languages for one module.** `re` is now Rust (`_sre`) + Python
  (`re._*`). That is precisely CPython's split, but it means a bug can
  live on either side of the FFI line; the saving grace is that the
  Python side is *CPython's own code*, so bugs concentrate in the small
  Rust core.
- **The surfaced fix tail was broad.** Half this RFC is interpreter
  fixes (slicing, int-subclassing, repr, `%`) that are *not* about
  regex. That is the nature of running real CPython code: it exercises
  the whole object model. Those fixes are pure upside elsewhere, but
  they widened the diff.
- **No native `Pattern`/`Match` type.** Code that introspects
  `type(p).__module__ == '_sre'` or pickles a compiled pattern via the
  C type's `__reduce__` sees our Python classes instead. The observable
  attribute/method surface matches; the type identity does not.

## Alternatives

1. **Keep the `regex`/`fancy_regex` shim and paper over differences.**
   Rejected: the differences are unbounded and silent, and `test_re.py`
   asserts on engine internals a shim can't reproduce. Every patch
   would be whack-a-mole against a foreign engine's choices.
2. **Port `_sre` *and* write `Pattern`/`Match` as native Rust types.**
   More faithful to CPython's type identity, but a large second native
   object surface (GC, attributes, repr, pickle) for marginal gain over
   frozen-Python classes. Deferred to future work if type-identity
   parity is ever required.
3. **Compile Python regex to the Rust `regex` crate's AST.** A
   translation layer by another name — same fidelity ceiling as the
   shim, plus a new impedance mismatch (no backtracking, different
   group semantics). Rejected.
4. **A bytecode-level `re` fast path in the VM.** Premature: get
   faithful first, optimise the hot `exec` loop later (see *Future
   work*).

## Prior art

- **CPython** is the reference; we port its engine rather than imitate
  it. The secret-labs engine (Fredrik Lundh) has been stable in shape
  since Python 1.6, which is what makes a verbatim parser/compiler port
  viable.
- **PyPy** reimplements `_sre` in RPython but keeps `_sre.py`'s
  structure and the CPython `re` Python layer — the same "port the C
  core, reuse the Python" strategy this RFC follows.
- **RustPython** ships a hand-written `sre-engine` Rust crate plus the
  CPython Python layer — close to our approach; our matcher independently
  arrives at the same `toplevel`/`must_advance` structure, which is good
  corroboration that it's the load-bearing invariant.
- **GraalPy** runs CPython's `_sre` Python over a Truffle-based engine;
  again, the Python layer is reused, not rewritten.

The cross-implementation consensus — *reuse CPython's Python `re`
layer, reimplement only the C core* — is exactly what this RFC adopts.

## Unresolved questions

- **`localeconv`/`LOCALE`-flag fidelity.** `IN_LOC_IGNORE`/
  `CATEGORY_LOC_*` depend on the C locale; we implement the structure
  but the locale tables are the byte locale only. Full locale parity is
  deferred (CPython itself discourages `re.LOCALE` on str patterns).
- **Native-stack depth vs `sys.setrecursionlimit`.** Should the matcher
  consult the Python recursion limit to raise `RecursionError` instead
  of risking a native overflow on adversarial input?
- **Pickling compiled patterns.** Do we need `Pattern.__reduce__` to
  round-trip through `re.compile(pattern, flags)` (CPython's approach)
  before any real workload needs it?

## Future work

- **Optimise the `exec` hot loop.** The faithful matcher is correctness-
  first; a charset-prefix fast path and a flattened dispatch (or a
  Tier-2 JIT intrinsic per RFC 0032) can follow now that behaviour is
  pinned by `test_re.py`.
- **Native `Pattern`/`Match` types** if/when `type` identity or C-level
  pickling parity is required.
- **Wire the full `Lib/test/test_re.py`** (including the C-detail
  refleak/`gc` hooks) into the RFC 0034 opt-in CPython sweep, not just
  the bundled subset.
- **`regex`-module-style atomic-group/possessive optimisations** kept
  behind CPython-compatible semantics.
- **Locale tables** for `re.LOCALE` parity on bytes patterns.

[`regex`]: https://docs.rs/regex
[`fancy_regex`]: https://docs.rs/fancy-regex
