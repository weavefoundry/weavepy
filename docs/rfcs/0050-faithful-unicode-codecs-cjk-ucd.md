# RFC 0050: Faithful Unicode — codec machinery completion, CJK multibyte codecs, UCD 15.1, locale/UTF-8 mode, and the severity-1 VM crashes

- **Status**: Draft
- **Authors**: WeavePy authors
- **Created**: 2026-07-11
- **Tracking issue**: TBD
- **Builds on**: RFC 0040 (WTF-8 `Object::WStr` storage + PEP 383
  `surrogateescape` filenames), RFC 0019 (`_codecs` native module),
  RFC 0049 (wave-5 measured whole-suite baseline + the six built-in
  error-handler callables).

## Summary

Close the Unicode/codec failure cluster — the largest single cluster
in the RFC 0049 measured baseline — by finishing the codec machinery
that RFC 0040's WTF-8 storage arc made possible but did not complete.
Concretely: (1) complete the `codecs` module surface to the documented
CPython 3.13 API (per-codec `_codecs` entry points, `EncodedFile`,
`StreamRecoder`, faithful `StreamReader`/`StreamWriter` semantics,
custom error-handler callbacks honored by the *native* encode/decode
engines); (2) ship the CJK multibyte codec stack — a native
`_multibytecodec` plus `_codecs_cn/_codecs_jp/_codecs_kr/_codecs_tw/
_codecs_hk/_codecs_iso2022` backed by mapping tables generated from
host CPython 3.13, and the ~20 missing `encodings.*` wrapper modules
vendored verbatim; (3) re-base `unicodedata` (and the str-method case
tables that feed `test_stringprep` and identifier rules) on generated
UCD **15.1.0** tables, eliminating the measured Unicode-16.0 crate
skew; (4) finish PEP 540 UTF-8 mode (stdio `surrogateescape`/
`backslashreplace` error defaults, `PYTHONUTF8`) and give `_locale` a
real libc backend; and (5) fix the three severity-1 VM crashes the
baseline records (`env::args()` panic on non-UTF-8 argv, `eval`/`exec`
on free-variable code, and `f_lineno` trace jumps that never move the
program counter).

## Motivation

The RFC 0049 baseline names Unicode/codecs as the largest failure
cluster and repeatedly annotates rows with "deferred to the WTF-8
arc". The storage half of that arc landed in RFC 0040 — `chr(0xD800)`,
lone-surrogate literals, `os.fsencode` PEP 383 round-trips all work —
but the codec *machinery* half never followed. The measured cost
today:

- `cpython/Lib/test/test_codecs.py`: 284 tests, **118 fail + 226
  error** (measured on `main`, 2026-07-11). The failures are not
  storage failures; they are missing module attributes
  (`codecs.utf_7_decode`, `unicode_escape_decode`,
  `readbuffer_encode`, `charmap_build`, `EncodedFile`,
  `StreamRecoder`, `encodings._cache`), stream-codec semantics
  (`StreamReader.readline(keepends=)`, partial-input buffering,
  `seek`/`reset` state), strictness gaps ("`UnicodeDecodeError` not
  raised" × 38), and custom `register_error` handlers that the native
  fast paths never consult.
- The seven `test_codecencodings_*`/`test_multibytecodec` labels and
  five `test_codecmaps_*` labels are red for one reason: the CJK
  codec stack does not exist (`ModuleNotFoundError:
  _multibytecodec`, `LookupError: unknown encoding: big5hkscs`, …).
- `test_unicodedata` is *skipped* for UCD-version skew: the engine
  ships Unicode 16.0.0 via the `unicode-properties`/`unicode_names2`
  crates while CPython 3.13 pins **15.1.0**. A conformance suite
  graded against CPython 3.13 must match CPython 3.13's database.
- `test_utf8_mode` fails on stdio error handlers (`utf-8/strict`
  where CPython reports `utf-8/surrogateescape`), and `PYTHONUTF8`
  is documented in `--help-env` but never read.
- `test__locale` fails because `_locale` is a C-locale stub
  (`localeconv()['decimal_point']` is always `"."`).

Beyond the label count: `surrogateescape` is CPython's filesystem
error handler on POSIX. Real programs hit these paths whenever argv,
environment variables, or filenames contain bytes that are not valid
UTF-8 — and today a non-UTF-8 argv **panics the VM** (`test_cmd_line`,
severity 1). A drop-in replacement cannot crash where CPython
degrades gracefully. The two other recorded InternalError crashes
(`test_scope`, `test_sys_settrace`) ride along in this RFC because
engine-integrity bugs should not wait for a thematic wave of their
own.

## CPython reference

- **Codec machinery**: `Modules/_codecsmodule.c`,
  `Python/codecs.c` (error-handler registry + built-in handlers),
  `Lib/codecs.py` (`EncodedFile`, `StreamRecoder`, `StreamReader`
  buffering semantics), `Lib/encodings/__init__.py` (search
  function + `_cache`). Docs: `Doc/library/codecs.rst`. Tests:
  `Lib/test/test_codecs.py`, `Lib/test/test_codeccallbacks.py`,
  `Lib/test/test_charmapcodec.py`.
- **Error handlers**: PEP 293 (codec error callbacks), PEP 383
  (`surrogateescape`), `surrogatepass` in
  `Python/codecs.c:PyCodec_SurrogatePassErrors` (UTF-8/16/32
  variants).
- **CJK codecs**: `Modules/cjkcodecs/` (`multibytecodec.c`, the
  `_codecs_cn/jp/kr/tw/hk/iso2022` modules and their `*.map`
  tables), `Lib/encodings/{gbk,gb2312,gb18030,big5,big5hkscs,cp932,
  cp949,cp950,euc_jp,euc_kr,euc_jis_2004,euc_jisx0213,shift_jis,
  shift_jis_2004,shift_jisx0213,johab,hz,iso2022_jp,iso2022_jp_1,
  iso2022_jp_2,iso2022_jp_2004,iso2022_jp_3,iso2022_jp_ext,
  iso2022_kr}.py`. Tests: `Lib/test/test_multibytecodec.py`,
  `Lib/test/test_codecencodings_*.py`, `Lib/test/test_codecmaps_*.py`,
  data in `Lib/test/cjkencodings/`.
- **UCD**: `Modules/unicodedata_db.h` and `Tools/unicode/makeunicodedata.py`
  (CPython generates its database; we generate ours the same way, from
  the same pinned UCD 15.1.0 via host CPython 3.13),
  `Doc/library/unicodedata.rst`. Tests: `Lib/test/test_unicodedata.py`,
  `Lib/test/test_ucn.py`, `Lib/test/test_stringprep.py` (RFC 3454 over
  UCD 3.2 tables in `Lib/stringprep.py`).
- **UTF-8 mode / locale**: PEP 540, PEP 538,
  `Python/preconfig.c` (`PYTHONUTF8`, `-X utf8`),
  `Python/pylifecycle.c` (stdio error-handler selection),
  `Modules/_localemodule.c`. Tests: `Lib/test/test_utf8_mode.py`,
  `Lib/test/test__locale.py`.
- **Crashes**: `Python/getargs.c`/`Modules/main.c` argv decoding
  (`Py_DecodeLocale`, PEP 383); `Python/ceval.c` `eval`/`exec`
  free-variable guard (`code object passed to exec() may not contain
  free variables`); `Objects/frameobject.c:frame_setlineno` (the
  authoritative "set next line" semantics: line→offset mapping,
  block-stack reconciliation, the exhaustive "can't jump into/out
  of…" error taxonomy). Tests: `Lib/test/test_cmd_line.py`,
  `Lib/test/test_scope.py`, `Lib/test/test_sys_settrace.py`
  (`JumpTestCase`).

## Current baseline (measured starting point)

From `tests/regrtest/expectations.toml` (RFC 0049 sweep) and fresh
probes on `main` (2026-07-11):

| Label | Status | First failure |
|-------|--------|---------------|
| `test_codecs.py` | fail | 118 F / 226 E of 284 — module surface + stream semantics + strictness |
| `test_codeccallbacks.py` | fail | custom handlers unseen by native paths; `codecs.charmap_build` missing |
| `test_codecencodings_{cn,hk,iso2022,jp,kr,tw}.py` | fail ×6 | `LookupError: unknown encoding` / `ModuleNotFoundError: _multibytecodec` |
| `test_codecmaps_{cn,hk,jp,kr,tw}.py` | fail ×5 | same |
| `test_multibytecodec.py` | fail | `ModuleNotFoundError: No module named '_multibytecodec'` |
| `test_charmapcodec.py` | fail | charmap codec surface |
| `test_unicodedata.py` | skip | UCD 16.0 vs 15.1 skew + NormalizationTest budget |
| `test_ucn.py` | fail | `\N{...}` name-alias/named-sequence gaps (UCD skew) |
| `test_stringprep.py` | fail | RFC 3454 table drift (case/property tables) |
| `test_utf8_mode.py` | fail | stdio `strict` where CPython has `surrogateescape`; `PYTHONUTF8` unread |
| `test__locale.py` | fail | `_locale` is a C-locale stub (`'.' != ','`) |
| `test_cmd_line.py` | fail | **panic**: `env::args()` unwraps non-UTF-8 argv |
| `test_scope.py` | fail | **InternalError**: `bad cell index` — `eval(g.__code__)` with freevars |
| `test_sys_settrace.py` | fail | **InternalError**: `FOR_ITER no iter` — `f_lineno` jumps are cosmetic |

Adjacent rows expected to improve but not gating: `test_str.py`
(encoding-edge residuals), `test_io.py` edges, `test_repl`/`test_site`
stdio-encoding interactions.

## Detailed design

### WS1 — Severity-1 VM crashes · ~1.5K LOC

1. **Non-UTF-8 argv** (`crates/weavepy-cli/src/main.rs:530,679`).
   Replace `env::args()` with `env::args_os()` and decode each
   argument with UTF-8 + `surrogateescape` (PEP 383, matching
   `Py_DecodeLocale` under UTF-8 mode) into the existing
   `Str`/`WStr`-aware constructor path. `sys.argv` entries containing
   undecodable bytes become `WStr` values that round-trip through
   `os.fsencode`, exactly as CPython. The same helper feeds the
   multiprocessing child re-exec path. `Interpreter::set_argv`
   (`weavepy-vm/src/lib.rs`) grows a code-point-aware variant.
2. **`eval`/`exec` on free-variable code**
   (`weavepy-vm/src/lib.rs`, `do_eval_call`/`do_exec_call`). Guard
   before `make_frame`: if the code object has non-empty
   `co_freevars`, raise `TypeError("code object passed to eval() may
   not contain free variables")` (resp. `exec()`), CPython-verbatim.
3. **Real `f_lineno` assignment** (`weavepy-vm/src/lib.rs` frame
   attribute setter + `weavepy-vm/src/object.rs` frame state).
   Implement CPython's `frame_setlineno`: only legal from a `'line'`
   trace event; map the target line to a bytecode offset via the line
   table; compute the value/block-stack depth delta between source
   and target using the same exception-table/stack-effect analysis
   the compiler already has; refuse illegal jumps with CPython's
   error messages (into/out of `for` iterators except backward jumps
   that re-`GET_ITER`… — follow 3.13's rules: pop iterators and exit
   `with`/`finally` blocks when jumping out, forbid jumping *into*
   any block); set the live frame `pc`, truncate/adjust the stack,
   clear `override_lineno`. `FOR_ITER`'s empty-stack InternalError
   becomes unreachable through legal jumps; keep it as a hard
   invariant.

### WS2 — `codecs`/`_codecs` module surface completion · ~4K LOC

Bring `crates/weavepy-vm/src/stdlib/codecs_mod.rs` +
`stdlib/python/codecs.py` to the documented 3.13 surface:

1. **Per-codec `_codecs` entry points**, re-exported by `codecs`:
   `utf_7_encode/decode`, `utf_8_encode/decode`, `utf_16_*`,
   `utf_32_*` (+ `_le`/`_be`/`_ex` variants with the
   `(result, consumed[, byteorder])` tuple shapes),
   `latin_1_*`, `ascii_*`, `charmap_encode/decode/build`,
   `unicode_escape_encode/decode`, `raw_unicode_escape_*`,
   `readbuffer_encode`, `escape_encode/decode`. Each takes the
   standard `(input, errors=None[, final])` signature and returns
   CPython's exact tuple shape.
2. **UTF-7 as a real first-class codec** (encode + incremental
   decode with base64-state carryover), replacing the `encoding_rs`
   passthrough that cannot represent partial-shift state.
3. **Stream layer**: `codecs.EncodedFile`, `codecs.StreamRecoder`,
   and faithful `StreamReader` semantics — `readline(size, keepends)`,
   `read(size, chars, firstline)`, charbuffer/bytebuffer carry,
   `seek`/`reset`. These are pure-Python in CPython's `Lib/codecs.py`;
   we adopt the verbatim implementations and make the incremental
   codec objects they depend on real.
4. **Error-handler unification.** Native encoders/decoders currently
   hardcode the known handlers. Restructure to CPython's model: the
   engine attempts the fast path; on error it *looks up the handler
   by name* (built-ins dispatch to native implementations; anything
   else calls the registered Python callable with a real
   `UnicodeEncodeError`/`UnicodeDecodeError`/`UnicodeTranslateError`
   carrying `object`/`start`/`end`/`reason`), applies the
   `(replacement, new_position)` result — including **bytes**
   replacements on encode (PEP 383 requires it) and backward
   positions — and continues. This single change is what
   `test_codeccallbacks.py` actually tests.
5. **Strictness sweep**: the 38 "`UnicodeDecodeError` not raised"
   rows — UTF-8 (reject surrogates and over-long forms in strict
   mode; `surrogatepass` accepts *only* the exact CESU-8 forms),
   UTF-16/32 (truncated data, `\ud800` strict-encode rejection),
   and the escape codecs (trailing `\`, `\x` without two hex
   digits → `ValueError` with CPython's messages, invalid-escape
   `DeprecationWarning` parity).
6. **`encodings` package internals**: `encodings._cache`,
   `encodings._unknown`, `search_function` shape,
   `encodings.normalize_encoding` (the tests import these), plus
   `codecs.lookup()` returning a real populated `CodecInfo` with
   `_is_text_encoding`.

### WS3 — `_multibytecodec` + CJK codec modules · ~6K LOC + generated tables

The centerpiece. Native `crates/weavepy-vm/src/stdlib/
multibytecodec_mod.rs` implementing CPython's
`Modules/cjkcodecs/multibytecodec.c` object model:

- `MultibyteCodec` (opaque codec handle with `encode`/`decode`),
  `MultibyteIncrementalEncoder`, `MultibyteIncrementalDecoder`
  (with `getstate`/`setstate` returning CPython's shapes),
  `MultibyteStreamReader`, `MultibyteStreamWriter`.
- Error-handler integration goes through the WS2 unified callback
  machinery (CJK tests exercise custom handlers heavily).

Codec engines, one per family, in
`crates/weavepy-vm/src/stdlib/cjkcodecs/`:

- **cn**: gb2312, gbk, gb18030 (incl. the 4-byte linear region),
  hz (stateful `~{`/`~}` shifts).
- **jp**: cp932, euc_jp, euc_jis_2004, euc_jisx0213, shift_jis,
  shift_jis_2004, shift_jisx0213, and the stateful
  iso2022_jp/_1/_2/_2004/_3/_ext family (escape-sequence state
  machine shared with **iso2022** kr).
- **kr**: euc_kr (incl. the 8-byte UHC extension semantics of
  cp949), cp949, johab, iso2022_kr.
- **tw**: big5, cp950.
- **hk**: big5hkscs (incl. the U+00CA-style double-codepoint
  mappings and the `\N{...}` compatibility points that WHATWG big5
  gets wrong).

Mapping tables are **generated from host CPython 3.13** by a new
`tools/gen_cjk_tables.py`: for every codec, exhaustively round-trip
the byte space and the BMP+SIP code-point space through CPython's
codec, emitting dense range-compressed decode tables and sorted
encode tables as packed `&'static [u8]` blobs (`include_bytes!` from
`crates/weavepy-vm/src/stdlib/cjkcodecs/tables/*.bin`, with a small
loader; regeneration is a checked-in, reproducible script, mirroring
CPython's own `Tools/unicode/` generation story). `encoding_rs` stays
for the WHATWG single-byte encodings but is **no longer consulted for
CJK** — its WHATWG semantics diverge from CPython's tables precisely
where the tests look.

The ~24 `encodings/*.py` CJK wrapper modules are vendored verbatim
from CPython (`getregentry()` over `_codecs_*.getcodec` +
`_multibytecodec` incremental/stream classes) and added to
`frozen_sources()`.

### WS4 — `unicodedata` on UCD 15.1.0 · ~2K LOC + generated tables

Replace the crate-backed engine in
`stdlib/unicodedata_mod.rs` with generated tables pinned to CPython
3.13's database, produced by `tools/gen_ucd_tables.py` running under
host CPython 3.13 (whose `unicodedata` *is* UCD 15.1.0):

- Per-code-point record (category, bidirectional, combining,
  mirrored, east_asian_width, decimal/digit/numeric, decomposition
  type+mapping) in the two-level trie layout CPython itself uses
  (`makeunicodedata.py` splitbins), packed via `include_bytes!`.
- `name()`/`lookup()` from a generated name index (replacing
  `unicode_names2`), including PEP 3131 name aliases and named
  sequences (`test_ucn` exercises both) and the `ucd_3_2_0` snapshot
  object (delta table against 3.2, as CPython ships).
- NFC/NFD/NFKC/NFKD + `is_normalized` quick-check from the same
  tables (replacing `unicode-normalization` in the module; the
  parser's NFKC identifier fold switches to the same source of
  truth).
- Retire `unicode_decomp_data.rs` and the crate deps from the
  module; `unidata_version` becomes `"15.1.0"`.
- Feed the same generation pass into the str-method tables that
  `test_stringprep` and `str.title()/casefold()/isidentifier()`
  depend on (`XID_Start`/`XID_Continue`, Cased/Case_Ignorable,
  full case folding), replacing the ad-hoc `casefold_char()` overlay.

### WS5 — UTF-8 mode, stdio encoding, and real `_locale` · ~2K LOC

1. **`PYTHONUTF8`** read in `weavepy-cli` `EnvOverrides::from_env()`
   (respecting `-E`/`-I`), combined with `-X utf8` per PEP 540
   precedence, driving `sys.flags.utf8_mode`.
2. **Stdio error handlers**: under UTF-8 mode, `sys.stdin`/`stdout`
   get `errors="surrogateescape"` and `sys.stderr`
   `errors="backslashreplace"` (CPython always uses
   `backslashreplace` for stderr regardless of mode);
   `PYTHONIOENCODING` (`encoding[:errors]`) overrides both. Fix
   `PyFile::errors_name()` defaults and the `encoding`/`errors`
   reporting on the std streams.
3. **Real `_locale`** (`stdlib/locale_mod.rs`): back
   `setlocale`/`localeconv`/`nl_langinfo`/`strcoll`/`strxfrm` with
   libc (the `libc` crate is already a dependency), including the
   `localeconv` grouping/decimal-point fields `test__locale` checks
   across the locales it probes (skipping gracefully where the host
   lacks a locale, as CPython's test does). `locale.getpreferredencoding`
   and `locale.getlocale` become locale-aware, with PEP 538/540
   coercion semantics preserved.

### WS6 — Fixtures + measured baseline rewrite · ~1K LOC

- New bundled regrtests: WTF-8 argv/env round-trip (spawns a child
  with non-UTF-8 argv), CJK codec round-trip against
  `Lib/test/cjkencodings/` data, error-handler callback protocol,
  UCD version + `name`/`lookup` spot checks, stdio error-handler
  matrix, and a `f_lineno` jump matrix mirroring `JumpTestCase`
  shapes.
- Re-run the full sweep (`--all-cpython --no-check --mode subprocess
  --jobs 8`) and rewrite the touched rows of
  `tests/regrtest/expectations.toml` to the new measured reality —
  same policy as RFC 0049: green rows lose their entry, red rows
  carry the measured first failure.

## Measured targets

Labels this RFC intends to flip to `pass` (from the baseline table
above): `test_codecs`, `test_codeccallbacks`, `test_charmapcodec`,
`test_codecencodings_{cn,hk,iso2022,jp,kr,tw}`,
`test_codecmaps_{cn,hk,jp,kr,tw}`, `test_multibytecodec`,
`test_unicodedata` (un-skipped; NormalizationTest may stay
budget-capped — if so the row records that explicitly),
`test_ucn`, `test_stringprep`, `test_utf8_mode`, `test__locale`,
and the three crash rows (`test_cmd_line`, `test_scope`,
`test_sys_settrace`) at minimum crash-free with measured residuals.
That is **up to 20 labels**, plus expected residual improvements in
`test_str`, `test_io`, and the CLI family.

Hard acceptance floor: zero Rust panics / InternalErrors across the
whole sweep; `--check` green against the rewritten baseline; no
regression on any currently-green row; `cargo fmt` + `clippy -D
warnings` + full workspace tests green.

## Non-goals / Drawbacks

- **No PEP 393 storage rewrite.** RFC 0040's dual `Str`/`WStr`
  representation stays; this RFC closes machinery gaps, not storage.
  The C-API mirror already fabricates PEP 393 bodies for extensions.
- **`test_locale` (the frozen `locale.py` suite) stays skipped** —
  it needs `setlocale` to succeed for specific locales in CI; WS5
  makes `_locale` real but the row stays measured, not promised.
- **No `aliases.py` beyond CPython's.** Alias behavior is vendored,
  not extended.
- Generated tables add ~1–2 MB of packed data to the binary. CPython
  carries the same data (`unicodedata` ≈ 1.2 MB, cjkcodecs maps
  ≈ 1 MB); we accept the size for fidelity. Regeneration requires a
  host CPython 3.13, documented in the tool headers.
- The `f_lineno` jump implementation is the riskiest piece (stack
  reconciliation); it is deliberately scoped to CPython 3.13's legal
  jump set, with illegal jumps raising CPython's exact errors rather
  than attempting best-effort execution.

## Alternatives

- **Keep `encoding_rs` for CJK.** Rejected: WHATWG mapping semantics
  (big5 vs cp950, euc-jp extension rows, gb18030-2022 updates)
  diverge from CPython's tables exactly where `test_codecmaps_*`
  looks; no incremental-state surface compatible with
  `_multibytecodec`'s `getstate`/`setstate` contract.
- **Port CPython's C `cjkcodecs` verbatim.** The table *data* is
  what matters; the C state machines are small. Generating tables
  from the host oracle gives byte-identical behavior with far less
  transliteration risk than porting `_codecs_iso2022.c` line by line.
- **Upgrade conformance to Unicode 16 instead of downgrading tables.**
  Rejected: CPython 3.13 is the spec; its tests hardcode 15.1.0
  behavior (`test_ucn` aliases, East Asian widths).
- **Leave the crashes to a dedicated hardening RFC.** Rejected:
  two of the three are in this RFC's blast radius anyway (argv
  decoding is PEP 383; settrace jumps gate `test_sys_settrace`,
  which the observability arc of RFC 0031 already claims).

## Prior art

- **CPython** generates both its UCD and CJK tables
  (`Tools/unicode/makeunicodedata.py`, `Tools/unicode/genmap_*.py`);
  this RFC adopts the same generate-don't-transcribe philosophy with
  CPython itself as the oracle.
- **PyPy** implements `_multibytecodec` in RPython over the same
  CPython-derived map files — evidence that the object model ports
  cleanly without the C code.
- **RustPython** vendors `unicode_names2`/`unicode-casing` crates and
  carries known UCD-version skew against CPython — the exact trap
  this RFC removes.

## Unresolved questions

- Whether `test_unicodedata`'s full `NormalizationTest` sweep fits
  the per-test budget once tables are native (expected: yes in
  release; the row records the measured answer).
- How far `test__locale` can go in CI where only `C`/`POSIX`/UTF-8
  locales are installed (the test skips per-locale; the row records
  the measured verdict).
- Whether `iso2022_jp_2004`/`_3` corner mappings (JIS X 0213 plane-2)
  need the `euc_jis_2004` frozen fallback retired or kept as the
  table source.

## Future work

- The remaining `test_codecs` locale-codec rows that require
  `_testinternalcapi.EncodeLocaleEx`/`DecodeLocaleEx` (C-API test
  shims, not codec behavior).
- `str` method UCD fidelity beyond stringprep's needs (full
  `str.title()` Cased/Case_Ignorable rules) — started here, finished
  alongside the `test_str` residual cluster.
- PEP 686 (UTF-8 mode by default in 3.15) tracking.
- The wave-6 conformance sweep of the core-language cluster
  (`test_builtin`, `test_syntax`, compiler/dis parity) — explicitly
  *not* this RFC.
