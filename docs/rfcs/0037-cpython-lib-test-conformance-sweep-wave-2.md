# RFC 0037: CPython `Lib/test/` conformance sweep, wave 2 — root-cause clusters and verbatim module ports

- **Status**: Accepted
- **Authors**: WeavePy authors
- **Created**: 2026-06-02
- **Tracking issue**: TBD
- **Builds on**: RFC 0036 (wired a real CPython 3.13 `Lib/test/` checkout
  into `regrtest` and rewrote the touched `expectations.toml` rows from
  guesses to a *measured* baseline), RFC 0035 (faithful `re`/Unicode —
  same "port CPython verbatim where behaviour is defined by CPython"
  ethos), RFC 0033 (`ast`/`dis`/`marshal` introspection), RFC 0015
  (object-model completion).

## Summary

RFC 0036 made the CPython regression suite *runnable and measured*: with
`vendor/cpython/Lib/test/` checked out, `weavepy-conformance regrtest
--mode subprocess` now produces honest per-file verdicts, and the
committed baseline in `tests/regrtest/expectations.toml` is `--check`
clean. That baseline currently records **115 `fail` rows and 30 `skip`
rows** against the real suite — the long tail the README calls out as
"still being worked through file by file."

This RFC is **wave 2 of the sweep**: instead of grinding one test at a
time, it attacks the *shared root causes* that gate whole clusters of
those files, then ports the handful of stdlib modules whose absence
blocks the largest remaining groups. The work is organised into ten
workstreams (WS1–WS10). The throughline is unchanged from RFC 0035/0036:
**where behaviour is defined by CPython, port CPython** (verbatim Python
modules, faithful semantics) rather than re-approximate it.

The deliverable is measured, not aspirational: every workstream names the
`expectations.toml` rows it flips, and the commit is not done until a
fresh subprocess sweep is `--check` clean with those rows rewritten from
`fail`/`skip` to `pass`.

## Motivation

The README's headline promise is "a 100% compatible, drop-in replacement
for CPython … using CPython's own test suite as a guiding standard." RFC
0036 made that claim *auditable*; this RFC makes the audited number move.

The key observation from the RFC 0036 sweep — reaffirmed by re-reading
every `reason` field in `expectations.toml` — is that the 115 failures
are **not** 115 independent bugs. They cluster behind a small number of
missing primitives:

- A *single* missing recursion guard takes the **whole process down**
  (`abort()`/stack overflow) on any test that probes deep recursion, and
  in `--mode in-process` it can take the runner with it. `test_exceptions`
  is the named victim, but it also makes the in-process bundled runner
  fragile.
- A *single* ASCII-only identifier scanner blocks every file that uses a
  non-ASCII identifier (PEP 3131) — directly `test_unicode_identifiers`,
  and transitively any ported module that does.
- A *partial* `str.format`/`%` mini-language shows up as the first
  failure in `test_format`, `test_string`, `test_unicode`, and several
  numeric files.
- A *handful of absent modules* (`collections.abc`, `cmath`, `calendar`,
  `pydoc`, `locale`/`encodings`) are, per RFC 0036's own "non-goals"
  note, "the module-level dependencies gating the largest remaining
  clusters."

Fixing those primitives is worth far more than its line count, exactly
as RFC 0036 found when one parser fix (`[*a, *b]`) and one lexer fix
(`1.`) each unblocked multiple files at once.

## CPython reference

This RFC matches CPython 3.13 behaviour as defined by:

- **Recursion**: `sys.setrecursionlimit`/`getrecursionlimit`,
  `RecursionError`, and `Py_EnterRecursiveCall` / `Py_C_RECURSION_LIMIT`
  (CPython `Python/ceval.c`, `Include/cpython/pystate.h`). Tests:
  `Lib/test/test_exceptions.py` (`test_recursion*`),
  `Lib/test/test_sys.py` (`test_recursionlimit*`).
- **PEP 3131** — Unicode identifiers (`XID_Start`/`XID_Continue`,
  NFKC normalization of identifiers). Test:
  `Lib/test/test_unicode_identifiers.py`. Reference:
  `Lib/tokenize.py`, `Parser/pegen.c` `_PyPegen_normalize_name`.
- **PEP 701** — f-strings (backslashes in expressions, nested same-quote
  strings, multiline). Tests: `Lib/test/test_fstring.py`,
  `test_string_literals.py`.
- **Format mini-language** — `Lib/test/test_format.py`, the
  `Format Specification Mini-Language` docs, `str.__format__`,
  `Objects/unicodeobject.c` `format`, `Python/formatter_unicode.c`.
- **Numeric tower** — `Lib/test/test_complex.py`, `test_float.py`,
  `test_int.py`, `test_fractions.py`, `test_numeric_tower.py`; the
  `numbers` ABC hierarchy and `Lib/fractions.py`.
- **Exceptions** — PEP 654 (`ExceptionGroup`/`except*`), PEP 678
  (`BaseException.add_note`/`__notes__`), `Lib/traceback.py`.
- **Class/descriptor protocol** — PEP 487 (`__init_subclass__`,
  `__set_name__`), `Lib/test/test_descr.py`, `test_class.py`,
  `test_dataclasses.py`, `test_enum.py`.
- **Verbatim module ports** — `Lib/_collections_abc.py`,
  `Lib/cmath` (C module, ported as Python over the existing `math`
  core), `Lib/calendar.py`, `Lib/pydoc.py`, `Lib/locale.py`,
  `Lib/encodings/`.

Where this RFC ports a CPython `.py` file verbatim, it is pinned to the
3.13 branch tag already vendored under `vendor/cpython/`.

## Current baseline (measured starting point)

- `cargo build --workspace` is green.
- Bundled `tests/regrtest/` suite: **52/52 pass** in subprocess mode
  (`.scratch/ci.md`), `unexpected 0`.
- CPython `Lib/test/` allowlist in `expectations.toml`:
  **115 `fail`, 30 `skip`, 3 explicit `pass`** (512 test files vendored).

Wave 2 targets a coherent subset of those rows (see
[§Measured targets](#measured-targets)); the full tail remains a
multi-wave effort and this RFC does **not** claim to close it.

## Detailed design

Ten workstreams. Each lists the affected crate(s), the design, and the
`expectations.toml` rows it is expected to flip. Line-count estimates are
rough and include ported CPython `.py` plus the Rust glue/tests.

### WS1 — Recursion guard (`weavepy-vm`) · ~1.5K LOC

**Problem.** `sys.setrecursionlimit` is a no-op and `getrecursionlimit`
returns a hardcoded `1000`:

```609:616:crates/weavepy-vm/src/stdlib/sys.rs
fn sys_getrecursionlimit(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Int(1000))
}

fn sys_setrecursionlimit(args: &[Object]) -> Result<Object, RuntimeError> {
    let _ = args;
    // No-op for now: the host stack does the bounding.
    Ok(Object::None)
}
```

Deep Python recursion therefore overflows the *native* Rust stack and
`abort()`s the process instead of raising `RecursionError`.

**Design.**
- Add a per-thread `recursion_depth: Cell<usize>` and `recursion_limit:
  Cell<usize>` to the interpreter/thread state (alongside the existing
  per-thread handles in `vm_singletons`).
- Increment/decrement around every Python frame entry in the call path
  (the `CALL`/`CALL_FUNCTION_EX`/method-dispatch sites and the
  generator/coroutine resume sites in `lib.rs`). On exceeding the limit,
  raise `RecursionError("maximum recursion depth exceeded")` with the
  "while normalizing an exception" variant when already unwinding.
- Mirror CPython's "low-water" reset behaviour so the handler itself can
  run (`_Py_RecursionLimitLowerWaterMark`): allow a small overshoot for
  the error path.
- Wire `sys.setrecursionlimit`/`getrecursionlimit` to the per-thread
  field; validate the argument (`> 0`, fits the current depth).
- Independently, keep a coarse **native-stack** guard (probe remaining
  stack via `stacker`-style check, or a generous frame-count ceiling)
  so C-level recursion through dunder dispatch can't still overflow.

**Flips:** `test_exceptions` (recursion case), and hardens the runner so
later workstreams' tests fail cleanly instead of aborting. Contributes
to `test_sys` once that's in the allowlist.

### WS2 — Lexer/parser language gaps (`weavepy-lexer`, `weavepy-parser`) · ~3K LOC

**WS2a — PEP 3131 Unicode identifiers.** `is_ident_start` is ASCII-only:

```661:663:crates/weavepy-lexer/src/scanner.rs
fn is_ident_start(b: u8) -> bool {
    b == b'_' || b.is_ascii_alphabetic()
}
```

The continue path already uses `unicode_ident::is_xid_continue` (scanner
line 297), so the dependency is present. Make `is_ident_start` decode the
next UTF-8 scalar and test `unicode_ident::is_xid_start` (plus the legacy
`_`), and NFKC-normalize identifier text at token creation to match
CPython's `_PyPegen_normalize_name`. **Flips:** `test_unicode_identifiers`.

**WS2b — PEP 701 f-strings.** f-string lexing spans
`weavepy-lexer/src/token.rs` and parsing spans `weavepy-parser`.
Close the known gaps: backslashes inside f-string expression parts,
nested same-quote string literals, and multiline expressions. **Flips:**
`test_fstring`; contributes to `test_codecs`, `test_string_literals`.

**WS2c — String-literal escapes.** `eval` of some escape forms raises
(octal/edge escapes, per the measured `test_string_literals` reason).
Audit the unescape routine against CPython's `decode_unicode_with_escapes`
(`\xhh`, `\ooo`, `\N{NAME}` — already added in RFC 0036, `\uXXXX`,
`\UXXXXXXXX`, deprecation-warning-but-accept for unknown escapes).
**Flips:** `test_string_literals`.

**WS2d — `FOR_ITER` edge.** The measured `test_complex` reason cites a
`FOR_ITER` edge. Reproduce, fix the opcode handler in `lib.rs`. Folded in
with WS3 since it surfaces there.

### WS3 — Numeric tower (`weavepy-vm`, frozen `fractions`/`numbers`) · ~3K LOC

Measured reasons across `test_complex`, `test_float`, `test_int`,
`test_fractions`, `test_numeric_tower`:

- `Fraction("1.2")` must accept decimal/scientific string literals
  (CPython's `_RATIONAL_FORMAT` regex). Fix the frozen `fractions.py`
  parser.
- `complex` repr/format edge cases (`Objects/complexobject.c`
  `complex_repr`, `__format__`).
- `float.hex`/`float.fromhex` roundtrip + repr corner cases.
- `int` methods + `sys.int_info` struct-sequence shape (PEP 467 helpers,
  `bit_count`, `is_integer`).
- Port `Lib/numbers.py` (the `Number`/`Complex`/`Real`/`Rational`/
  `Integral` ABCs) so `isinstance(x, numbers.Integral)` etc. are correct;
  this also underpins WS8's `cmath`/`statistics`.

**Flips:** `test_complex`, `test_float`, `test_int`, `test_fractions`,
`test_numeric_tower`; contributes to `test_decimal`/`test_statistics`.

### WS4 — String/bytes formatting (`weavepy-vm`) · ~2.5K LOC

The format mini-language and `%`-formatting are partial (`test_format`,
`test_string`, `test_unicode`, `test_format`'s interop with `str.format`).

- Complete `str.__format__`/`format()` mini-language: fill/align, sign,
  `#`, `0`, width, grouping (`,` and `_`), precision, and type codes for
  `int`/`float`/`str`/`complex` (`b/c/d/e/E/f/F/g/G/n/o/s/x/X/%`).
- Complete `%`-formatting (`printf`-style) for `str` and `bytes`,
  including `%r`/`%a`/`%c`, mapping keys, and `*` width/precision.
- `bytes`/`bytearray` `.translate`/`maketrans` table semantics
  (`test_bytes`).

**Flips:** `test_format`, `test_string`, `test_bytes`; contributes to
`test_unicode`.

### WS5 — Exceptions, notes, groups, tracebacks (`weavepy-vm`, frozen `traceback`) · ~2.5K LOC

- PEP 678 `BaseException.add_note` / `__notes__` (storage + display).
- PEP 654 `ExceptionGroup`/`BaseExceptionGroup` propagation and `.split`/
  `.subgroup`/`.derive` semantics; ensure `except*` lowering matches.
- `traceback` module: exception-chaining display, `StackSummary` format,
  `TracebackException` (port the relevant parts of `Lib/traceback.py`
  verbatim, building on WS1's frame data).

**Flips:** `test_exceptions` (notes/groups portion, with WS1),
`test_traceback`; contributes to `test_contextlib`.

### WS6 — Class / descriptor / metaclass machinery (`weavepy-vm`, frozen `dataclasses`/`enum`/`typing`/`abc`) · ~3.5K LOC

Measured reasons across `test_class`, `test_descr`, `test_subclassinit`,
`test_dataclasses`, `test_enum`, `test_isinstance`, `test_abc`,
`test_typing`:

- PEP 487 ordering: `__set_name__` is called on the new class's
  attributes in definition order *after* the class object exists, then
  `__init_subclass__` on the parent — get the ordering exactly right,
  including inheritance and metaclass interaction.
- Descriptor protocol edges: slot conflicts, data vs non-data descriptor
  precedence, `classmethod`/`staticmethod` chaining (`test_decorators`).
- `dataclasses`: `slots=True`, `kw_only`, `__init_subclass__` interplay.
- `enum`: `StrEnum`/`IntEnum` mixins, value re-use, `_missing_`.
- `abc`/`ABCMeta`: `register()` ordering + virtual-subclass cache
  invalidation; depends on WS8's `_abc`/`abc` fidelity.

**Flips:** `test_class`, `test_descr`, `test_subclassinit`,
`test_decorators`, `test_dataclasses`, `test_enum`, `test_abc`,
`test_isinstance`; contributes to `test_typing`.

### WS7 — Iterators / generators / coroutines (`weavepy-vm`, frozen `contextlib`) · ~2K LOC

- `generator.throw()` into `yield from`, `close()` during a `yield`
  (`test_generators`).
- Coroutine `send`/`throw`, `async with`/`async for` edges
  (`test_coroutines`).
- Async generator `aclose()`/`asend()` (`test_asyncgen`).
- `contextlib.asynccontextmanager` (measured-missing per
  `test_contextlib_async`) + `ExitStack.callback` semantics.
- `iter(callable, sentinel)` + `__length_hint__` (`test_iter`).

**Flips:** `test_generators`, `test_coroutines`, `test_asyncgen`,
`test_iter`, `test_contextlib_async`; contributes to `test_contextlib`,
`test_with`.

### WS8 — Verbatim stdlib module ports (frozen Python) · ~6K LOC

Per RFC 0036, these absent modules gate the largest remaining clusters.
All confirmed missing from `crates/weavepy-vm/src/stdlib/python/`:

- **`collections.abc` / `_collections_abc`** — port `Lib/_collections_abc.py`
  verbatim and expose `collections.abc`. High fan-out: imported across
  the stdlib and by `typing`.
- **`cmath`** — port as Python over the existing `math` Rust core (or a
  thin Rust module) with correct branch cuts. **Flips:** unblocks files
  importing `cmath`.
- **`calendar`** — port `Lib/calendar.py`. **Flips:** `test_calendar`.
- **`locale` / `encodings`** — minimal but faithful `locale` + the
  `encodings` package registry so `str.encode`/`open(encoding=…)` resolve
  through the standard path. Gates `test_locale` (currently skip) and
  parts of `test_codecs`.
- **`pydoc`** — port enough of `Lib/pydoc.py` for `help()`/`pydoc.render*`
  used by doctest/inspect-adjacent tests.
- **Gap-fills** in already-present modules surfaced by their tests:
  `copy`/`copyreg` (`__copy__`/`__deepcopy__` + memo, extension dispatch),
  `itertools` recipes, `functools` (`partial`/`lru_cache`/`singledispatch`
  edges), `struct` (`pack_into` bounds, `@` alignment), `codecs`
  (error handlers + incremental state), `json` (sort_keys/non-str keys).

**Flips:** `test_calendar`, `test_copy`, `test_copyreg`, `test_itertools`,
`test_functools`, `test_operator`, `test_struct`, `test_collections`;
unblocks `cmath`/`locale` importers; contributes to `test_codecs`,
`test_json`.

### WS9 — `test.support` helper gap-fill (frozen `test.support`) · ~1K LOC

Several files fail at *import* because a `test.support` helper isn't
ported yet (measured: `cannot import name 'patch_list' from
'test.support'` blocks `test_bdb`). Port the missing helpers
(`patch_list`, and the others surfaced once WS1–WS8 let more files get
past import) into the frozen `test.support` package. This is pure
unblock-leverage: one helper can flip several files from `error` to a
real verdict.

**Flips:** `test_bdb`; unblocks additional files as discovered.

### WS10 — `expectations.toml` rewrite to measured truth · ~0.5K LOC (data)

After WS1–WS9, run `weavepy-conformance regrtest --cpython-dir
vendor/cpython/Lib/test --mode subprocess --jobs 8 --no-check` and
rewrite every touched row from `fail`/`skip` to its **measured** status,
quoting the first remaining failure for any row that doesn't reach
`pass`. The commit is complete only when a subsequent `--check` sweep
reports `unexpected 0`. New `bundled/` regression fixtures (one per
workstream) lock the behaviour in-process so CI catches regressions
without needing the full CPython checkout.

## Measured targets

Wave 2's commit-acceptance bar is flipping the following
`expectations.toml` rows to `pass` (grouped by workstream). Anything that
runs further but still fails gets a rewritten, measured `reason` rather
than a guess.

| Cluster | Target rows (→ `pass`) |
|---|---|
| WS1 recursion | `test_exceptions`* |
| WS2 lexer/parser | `test_unicode_identifiers`, `test_fstring`, `test_string_literals` |
| WS3 numerics | `test_complex`, `test_float`, `test_int`, `test_fractions`, `test_numeric_tower` |
| WS4 formatting | `test_format`, `test_string`, `test_bytes` |
| WS5 exceptions | `test_traceback`, `test_exceptions`* |
| WS6 classes | `test_class`, `test_descr`, `test_subclassinit`, `test_decorators`, `test_dataclasses`, `test_enum`, `test_abc`, `test_isinstance` |
| WS7 gen/coro | `test_generators`, `test_coroutines`, `test_asyncgen`, `test_iter`, `test_contextlib_async` |
| WS8 modules | `test_calendar`, `test_copy`, `test_copyreg`, `test_itertools`, `test_functools`, `test_operator`, `test_struct`, `test_collections` |
| WS9 support | `test_bdb` |

\* `test_exceptions` needs both WS1 (recursion) and WS5 (notes/groups).

That is **~30 files flipping `fail`/`skip` → `pass`** out of the 115/30,
plus measured-truth rewrites for everything that advances but doesn't
fully pass. The remaining tail (network/TLS live tests, C-accelerator
`pyexpat`/`_decimal`, `test_dict`/`test_list`/`test_set`/`test_tuple`
`sys.getsizeof`/refcount probes, `test_dis` opcode-table format,
`test_typing` PEP 695 corners, full `test_unicode`/`test_unicodedata`
UCD-version reconciliation) is explicitly **deferred to wave 3+**.

## Drawbacks

- **Breadth over depth risk.** Ten workstreams in one commit is a lot of
  surface; a regression in (say) the format mini-language could ripple.
  Mitigated by one bundled fixture per workstream and the `--check`
  baseline gate.
- **Verbatim ports carry CPython's own complexity.** `_collections_abc`,
  `calendar`, and `pydoc` pull in behaviour (and occasionally other
  imports) that may surface *new* gaps. We accept some scope creep within
  WS8 and cap it by deferring anything that needs an unshipped
  C-accelerator.
- **Recursion guard has a perf cost.** A per-frame depth check is one
  `Cell` increment/branch on the call path. Expected negligible vs. the
  existing eval-breaker poll; validated on the bench corpus before/after.
- **The headline number won't hit 100%.** This is one wave; we are
  explicit that ~30 files flip and the long tail continues.

## Alternatives

- **Grind file-by-file in test order.** Lower coordination cost but
  repeatedly re-discovers the same root causes (recursion, format,
  missing modules). Rejected: RFC 0036 already showed root-cause fixes
  dominate.
- **Pivot to the C-ABI binary-extension story (the other big "drop-in"
  lever).** Higher ceiling (real numpy/pandas) but much higher risk and
  poor fit for a single 20–30K LOC commit; sequenced as its own arc after
  this wave (see Future work).
- **Pivot to a faithful `asyncio`/selectors stack.** Coherent and
  valuable, but narrower than the cross-cutting conformance gains here;
  also deferred.

## Prior art

- **PyPy** runs (a fork of) CPython's `Lib/test` as its compatibility
  bar and ports CPython `.py` modules largely verbatim — the same
  strategy WS8 uses.
- **GraalPy/Jython** both found that the long tail is dominated by a few
  primitives (recursion handling, descriptor/`__set_name__` ordering,
  format mini-language) rather than exotic features — matching WeavePy's
  measured clustering.
- CPython's own `_collections_abc`, `fractions`, `calendar`, and
  `traceback` are pure Python and designed to be portable; reusing them
  verbatim is the lowest-divergence path.

## Unresolved questions

- **Native-stack guard mechanism.** Frame-count ceiling (portable,
  approximate) vs. real remaining-stack probing (`stacker`/platform
  APIs, precise but more `unsafe`). Lean frame-count first, revisit if a
  dunder-dispatch recursion still overflows.
- **NFKC normalization scope.** Normalize only identifiers (CPython) —
  confirm no other token path needs it.
- **`encodings` breadth.** How many codecs to register eagerly vs. lazily
  to keep startup fast and the frozen blob small.
- **Exact wave-2 cut line.** If WS6 (class machinery) proves deeper than
  estimated, split `test_typing`/`test_dataclasses` corners into wave 3
  rather than expand the commit.

## Future work

- **Wave 3 conformance**: the `sys.getsizeof`/refcount-dependent
  container tests, `test_dis` opcode-table format, UCD-version
  reconciliation (Unicode 16.0.0 vs 3.13's 15.1.0), PEP 695 `typing`
  corners, `decimal`/`pyexpat` C-accelerators.
- **Real CPython-ABI binary wheel loading** (the highest-ceiling
  drop-in lever): a dedicated RFC for `Py_LIMITED_API`/stable-ABI wheel
  loading and binary-compatible object layout, so unmodified scientific
  wheels load via `dlopen`.
- **Faithful `asyncio`**: wire all selector backends and unblock
  `test_asyncio`/`test_selectors`.
