# RFC 0041: C-accelerator numeric / data tower — faithful `math`, plus the deferred `_json` / `_csv` / `_datetime` / `_statistics` and container accelerators

- **Status**: Accepted
- **Authors**: WeavePy authors
- **Created**: 2026-06-25
- **Tracking issue**: TBD
- **Builds on**: RFC 0019 (numerics + serialization — the first numeric
  tower pass), RFC 0023 (drop-in parity — `array` ships), RFC 0036 (wired a
  real CPython 3.13 `Lib/test/` checkout into `regrtest` and rewrote the
  touched `expectations.toml` rows from guesses to a *measured* baseline),
  RFC 0037/0038 (the wave 2/3 conformance sweeps). RFC 0038 explicitly
  **defers `_json` and `_csv` to "the C-accelerator arc"** — this RFC is
  that arc. RFC 0039's perf wave made `test_statistics` complete-and-fail
  (rather than time out), exposing the numeric-tower residue this RFC
  closes.

## Summary

CPython's standard library leans on a tower of C accelerators for the
numeric and data-shuffling hot paths: `mathmodule.c`, `_json`, `_csv`,
`_datetime`, `_statistics`, and the `_heapq`/`_bisect`/`array` containers.
Several `Lib/test/` files don't just *use* these modules — they
**probe the C-vs-Python split directly** (binding `cjson.JSONDecodeError`,
calling `import_fresh_module(..., blocked=['_datetime'])`, running every
test class twice as a `C`/`Py` pair). A faithful Python surface is not
enough; the accelerator has to exist and match.

This RFC is a single coherent wave along that axis. **WS-math has already
landed** (this commit): the `math` module is now a faithful port of
CPython 3.13's `mathmodule.c` and `test_math.py` passes end-to-end. The
remaining workstreams ship the deferred accelerators (`_json`, `_csv`,
`_datetime`, `_statistics`) and close the container-accelerator edges
(`_heapq`/`_bisect`/`array`).

The throughline is unchanged from RFC 0035–0038: **where behaviour is
defined by CPython, port CPython** — verbatim Python modules where
practical, faithful semantics in the Rust accelerators otherwise — rather
than re-approximate it. The deliverable is measured, not aspirational:
each workstream names the `expectations.toml` rows it flips, is not done
until a fresh subprocess sweep is `--check` clean with those rows
rewritten from `fail` to `pass`, and lands at least one bundled in-process
fixture so CI catches regressions without the full CPython checkout.

## Motivation

The README's headline promise is "a 100% compatible, drop-in replacement
for CPython … using CPython's own test suite as a guiding standard." RFC
0036 made the number auditable; 0037/0038 moved it along the bounded tail.
The C-accelerator numeric/data tower is the next coherent cluster, and it
has two properties that make it the right target now:

1. **The failures are gated on a *missing or diverging accelerator*, not a
   missing subsystem.** The measured `fail` reasons are explicit about
   this: `test_json` import-crashes because `_json` is absent
   (`cjson.JSONDecodeError` can't bind); `test_csv` runs but the Rust
   `_csv` diverges (rejects non-list/tuple rows, dialect/typecode gaps,
   reader returns `None`); `test_datetime`'s `load_tests` can't satisfy the
   `_pydatetime` vs `_datetime` dual-module probe; `test_statistics`
   completes-and-fails on numeric-tower edges; `test_heapq`/`test_bisect`
   "pass core but stress tests probe [the] C accelerator." Each is a
   contained gap with a known root cause.

2. **The numeric tower is foundational.** `math`, the integer/float
   coercion path, and `sum()`/`**` are depended on transitively by
   `statistics`, `fractions`, `decimal`, and large swaths of user code.
   WS-math already surfaced and fixed three *core* arithmetic bugs (see
   below) that reach far beyond the `math` module — exactly the kind of
   shared root cause RFC 0036/0037 showed dominates the tail.

The higher-ceiling C-ABI binary-wheel arc (real `numpy`/`pandas` via
stable-ABI `dlopen`) remains the right *big* bet, but it is deep and
cross-cutting; this wave is its prerequisite (a faithful native numeric
tower) and a clean commit in its own right.

## CPython reference

This RFC matches CPython 3.13 behaviour as defined by the vendored
`vendor/cpython/Lib/` tree and the corresponding `Lib/test/` files:

- **math**: `Modules/mathmodule.c` (the `math_1`/`math_2` wrappers,
  `m_tgamma`/`m_lgamma`/`m_sinpi`, `m_erf`/`m_erfc`, `math_pow`/`fmod`/
  `frexp`/`modf`/`ldexp`/`remainder`/`nextafter`/`ulp`, `math_fsum`,
  `math_dist`/`hypot` `vector_norm`, `math_sumprod`, the `comb`/`perm`/
  `factorial` integer paths), `Lib/test/test_math.py` +
  `Lib/test/mathdata/{ieee754.txt,cmath.ctest,math_testcases.txt}`.
- **json**: `Modules/_json.c` (`make_scanner`, `make_encoder`,
  `scanstring`, `encode_basestring`, `encode_basestring_ascii`),
  `Lib/json/{decoder,encoder,scanner}.py`, `Lib/test/test_json/`.
- **csv**: `Modules/_csv.c` (the reader DFA, writer quoting, `Dialect`
  validation, `QUOTE_*`, `field_size_limit`), `Lib/csv.py`,
  `Lib/test/test_csv.py`.
- **datetime**: `Modules/_datetimemodule.c`, `Lib/_pydatetime.py`,
  `Lib/datetime.py`, `Lib/test/datetimetester.py` (driven by
  `test_datetime.py`).
- **statistics**: `Modules/_statisticsmodule.c` (`_normal_dist_inv_cdf`),
  `Lib/statistics.py`, `Lib/test/test_statistics.py`.
- **containers**: `Modules/_heapqmodule.c`, `Modules/_bisectmodule.c`,
  `Modules/arraymodule.c`, `Lib/heapq.py`/`Lib/bisect.py` (the pure-Python
  fallbacks the `C`/`Py` test pairs import), `Lib/test/{test_heapq,
  test_bisect,test_array}.py`.

Where this RFC ports a CPython `.py` file verbatim (`_pydatetime.py`), it
is pinned to the 3.13 branch tag already vendored under `vendor/cpython/`.

## Current baseline (measured starting point)

- `cargo build --workspace` is green; `cargo fmt` / `clippy` clean on the
  WS-math surface.
- Bundled `tests/regrtest/` suite is `--check` clean.
- CPython `Lib/test/` allowlist in `expectations.toml`, **after WS-math**:
  **88 `pass`, 43 `fail`, 21 `skip`, 1 `timeout`**. `test_math.py` is
  among the 88 (flipped this commit).

This RFC targets seven more `fail` rows (see [§Measured targets](#measured-targets));
the full tail remains a multi-wave effort and this RFC does **not** claim
to close it.

## Detailed design

Six workstreams plus fixtures and the baseline rewrite. Each lists the
affected crate(s)/module(s), the design, and the `expectations.toml` rows
it flips. Line-count estimates are rough and include ported CPython `.py`,
Rust glue, and tests.

### WS-math — faithful `mathmodule.c` (✅ landed this commit) · ~2K LOC

Rebuilt `crates/weavepy-vm/src/stdlib/math.rs` as a faithful port of
CPython's `mathmodule.c`:

- **3.12/3.13 surface**: added `math.fma` (with the IEEE 754 invalid-op /
  overflow error rules) and `math.sumprod` (the exact-`int` fast path, the
  `float`/`int` fast path, and the Python-`*`/`+` fallback that preserves
  `Fraction`/`Decimal` type and reproduces CPython's deliberate
  accumulation lossiness).
- **Special functions**: `gamma`/`lgamma` via the `g=6.024…, n=13`
  Lanczos rational approximation + `m_sinpi` reflection; `erf`/`erfc` via
  the power series + continued fraction. Both now sit within
  `test_mtestfile`'s few-ULP budget down to subnormals.
- **IEEE-754 domain/overflow**: `pow`, `fmod`, `frexp`, `modf`, `ldexp`,
  `remainder`, `nextafter(steps=)`, `ulp(FLOAT_MAX)` ported to CPython's
  exact special-case handling and `ValueError`/`OverflowError` messages.
- **High-accuracy reductions**: `fsum` (Shewchuk/Hettinger `msum` with the
  half-even fixup) and `hypot`/`dist` (`vector_norm` with double-length
  compensated arithmetic).
- **Exact integer paths**: `comb`/`perm`/`factorial`/`gcd`/`lcm` on
  `BigInt`; `loghelper` does huge-int `log`/`log2`/`log10` via a bigint
  `frexp`.
- **Protocol fidelity**: `ceil`/`floor`/`trunc` dispatch `__ceil__`/
  `__floor__`/`__trunc__`; the integer functions coerce via `__index__`
  and reject floats; `hypot`/`dist`/`fsum` accept `__float__` elements and
  iterate tuple subclasses / generators **through the VM**.

**Cross-cutting core fixes** the suite depends on (these are *not*
`math`-local and benefit `statistics`/`fractions`/user code):

- `builtins.sum()` now accumulates through the interpreter's binary
  dispatch (`op_binary`), so reflected `__radd__` fires — `sum([Fraction,
  …])` starts at `int` `0` and only `Fraction.__radd__` yields the sum. It
  also accepts `start=` as a keyword and rejects `str`/`bytes`/`bytearray`
  starts with CPython's "use `''.join(...)`" messages.
- `float(int)` coercion (`coerce_f64_opt`) raises `OverflowError: int too
  large to convert to float` for an over-range int instead of silently
  yielding `inf`.
- `0 ** -n` (int fast path) raises `ZeroDivisionError` instead of `inf`,
  and an `i64::MIN % -1` remainder-overflow **panic** is guarded (defers
  to bignum, mirroring the existing `FloorDiv` guard).

**Flipped:** `test_math` (86 ran, OK, 4 skipped).

### WS-json — native `_json` accelerator · ~2.5K LOC

Ship a native `_json` exposing the five symbols `Lib/json/` imports:
`make_scanner`, `make_encoder`, `scanstring`, `encode_basestring`,
`encode_basestring_ascii`. Wire `Lib/json/scanner.py` and `encoder.py` to
prefer the C path (`c_make_scanner`/`c_make_encoder`) and fall back to the
Python implementation when blocked, exactly as CPython does. This both (a)
unblocks the package import — `test.test_json` binds
`cjson.JSONDecodeError` against `_json`, which currently raises
`TypeError: 'NoneType' object has no attribute 'JSONDecodeError'` — and
(b) lets the suite's `CTest`/`PyTest` class pairs exercise each
implementation. Faithful edges to match: `scanstring`'s error offsets and
`\uXXXX` surrogate-pair handling, the encoder's `ensure_ascii`/`indent`/
`separators`/`sort_keys` interaction (CPython coerces then sorts mixed
keys), `parse_constant`/`object_pairs_hook`, and `JSONDecodeError`'s
`msg`/`pos`/`lineno`/`colno` attributes.

**Flips:** `test_json`.

### WS-csv — faithful `_csv` rewrite · ~2K LOC

Rewrite the Rust `_csv` accelerator against `Modules/_csv.c`. The measured
divergences are concrete: `writerow` must accept any iterable (not just
list/tuple) and stringify fields via the dialect, the reader must return
rows (not `None`) and drive the same parse DFA (quote/escape/in-field
states, `QUOTE_NONNUMERIC` float coercion, embedded newlines inside quotes),
`Dialect` construction must validate `delimiter`/`quotechar`/`escapechar`/
`lineterminator`/`quoting` with CPython's exact `TypeError`/`Error`
wording, and `field_size_limit([new])` must enforce + return the prior
limit. `Lib/csv.py` (`DictReader`/`DictWriter`/`Sniffer`) is already
Python; only the `_csv` core needs the rewrite.

**Flips:** `test_csv`.

### WS-datetime — `_pydatetime` + native `_datetime` split · ~6K LOC

`test_datetime.py`'s `load_tests` calls `import_fresh_module('datetime',
fresh=['datetime','_pydatetime','_strptime'], blocked=['_datetime'])` to
run the whole `datetimetester.py` suite **twice** — once on the pure-Python
implementation, once on the C one. WeavePy ships a single native
`datetime`, so the split can't be satisfied and `load_tests` errors before
any subtest runs. Port `Lib/_pydatetime.py` verbatim (the 3.13 pure-Python
reference) and restructure `Lib/datetime.py` to `from _datetime import *`
with the `from _pydatetime import *` fallback, so blocking `_datetime`
selects the Python path. Reconcile the native `_datetime` to the same
surface (`fold`, `tzinfo`/`timezone`, `fromisoformat`/`isoformat` 3.11+
extensions, `strftime`/`strptime` round-trips).

**Flips:** `test_datetime`.

### WS-statistics — native `_statistics` + numeric-tower fixes · ~1.5K LOC

Add the native `_statistics._normal_dist_inv_cdf` helper that
`statistics.NormalDist` uses, and close the residual numeric-tower gaps the
RFC 0039 perf wave exposed: `NormalDist` arithmetic, the weighted
`harmonic_mean` form, and `Fraction`/`Decimal` interop in
`mean`/`variance`/`fmean` (several of these lean directly on the WS-math
`sum()`/coercion fixes). `Lib/statistics.py` stays Python over the thin
native helper.

**Flips:** `test_statistics`.

### WS-containers — `_heapq` / `_bisect` / `array` edges · ~2K LOC

`test_heapq`/`test_bisect` "pass core but stress tests probe [the] C
accelerator": like `test_json`, they build `C`/`Py` pairs via
`import_fresh_module` and run the stress/randomized tests against both.
Ensure the native `_heapq`/`_bisect` exist with the exact signatures
(`heapify`/`heappush`/`heappop`/`heapreplace`/`heappushpop`/`_heapify_max`,
`bisect_left`/`bisect_right`/`insort_*` with `key=`/`lo`/`hi`) and that
blocking them selects the pure-Python fallback. For `array`: typecode range
checks (the 3.13 `'w'` UCS4 typecode included), `frombytes`/`tobytes`
round-trips, and the buffer-protocol edges (`memoryview` format/itemsize).

**Flips:** `test_heapq`, `test_bisect`, `test_array`.

### Fixtures + baseline rewrite · ~1K LOC (data)

After WS-json–WS-containers, run `weavepy-conformance regrtest
--cpython-dir vendor/cpython/Lib/test --mode subprocess --jobs 8
--no-check` and rewrite every touched row from `fail` to its **measured**
status, quoting the first remaining failure for any row that doesn't reach
`pass`. The commit is complete only when a subsequent `--check` sweep
reports `unexpected 0`. One bundled `tests/regrtest/bundled/` fixture per
workstream locks the behaviour in-process so CI catches regressions
without the full CPython checkout.

## Measured targets

The commit-acceptance bar is flipping the following `expectations.toml`
rows to `pass`. Anything that runs further but still fails gets a
rewritten, measured `reason` rather than a guess.

| Workstream | Target rows (→ `pass`) | Status |
|---|---|---|
| WS-math | `test_math` | ✅ landed |
| WS-json | `test_json` | queued |
| WS-csv | `test_csv` | queued |
| WS-datetime | `test_datetime` | queued |
| WS-statistics | `test_statistics` | queued |
| WS-containers | `test_heapq`, `test_bisect`, `test_array` | queued |

That is **8 files flipping `fail` → `pass`** (one landed, seven queued),
plus a measured-truth rewrite of `test_decimal`'s `skip` reason if the
native numeric work advances it. The C-ABI binary-wheel arc and a native
`_decimal` (libmpdec-equivalent) are explicitly **out of scope** here.

## Drawbacks

- **Accelerators duplicate a Python surface.** Shipping `_json`/`_csv`/
  `_datetime`/`_heapq`/`_bisect` natively means two implementations to keep
  in sync; the `C`/`Py` test pairs are the mitigation (they assert
  equivalence), as is one bundled fixture per workstream.
- **Verbatim `_pydatetime` is large** (~2.5K lines) and pulls in `time`/
  `_strptime` behaviour that may surface new edges. Capped by deferring
  anything that needs an unshipped accelerator.
- **Cross-cutting core changes carry blast radius.** The WS-math `sum()`/
  coercion/`**` fixes change *shared* interpreter behaviour. Mitigated by
  the regression sweep — `test_numeric_tower` (9), `test_fractions` (46),
  and `test_builtin::test_sum` were re-run green alongside `test_math`.
- **The headline number won't hit 100%.** This is one wave; 8 files flip
  and the long tail continues.

## Alternatives

- **Keep `math` as an approximation.** Rejected: `test_mtestfile`'s ULP
  budget and the `ieee754.txt`/`cmath.ctest` doctests make "close enough"
  measurably wrong, and `statistics`/`fractions` inherit the error.
- **Shim the test packages instead of shipping accelerators.** Binding a
  fake `cjson`/blocking-aware loader would flip the rows without the C
  path. Rejected: it defeats the purpose (the suite is *probing for the
  accelerator*), and the accelerators are the prerequisite for the
  binary-wheel arc anyway.
- **Fold these rows into the next generic sweep wave (0042).** Rejected:
  they share one root cause (the C/Py accelerator split) and one risk
  profile (numeric fidelity), so they make a more coherent, more reviewable
  commit as a dedicated wave — the same clustering argument from RFC
  0036/0037.

## Prior art

- **PyPy** ships its own `_json`/`_csv`/`_pydatetime` and runs CPython's
  `Lib/test` (including the `C`/`Py` pairs) as its bar — the same
  dual-implementation strategy this RFC adopts.
- **GraalPy/Jython** both report that numeric fidelity (gamma/erf ULPs,
  `fsum` rounding, `Fraction`/`Decimal` interop) and the accelerator-probe
  tests are a disproportionate slice of the long tail — matching WeavePy's
  measured clustering here.
- CPython's own `mathmodule.c`/`_json.c`/`_csv.c` are the reference
  implementations; this RFC ports their algorithms rather than re-deriving
  them.

## Unresolved questions

- **`_datetime` native vs. ported balance.** How much of
  `_datetimemodule.c` to keep native vs. let `_pydatetime` carry once the
  split exists — lean native for the hot constructors/arithmetic, Python
  for the formatting tail.
- **`_statistics` depth.** Whether `test_statistics` fully passes on
  `_normal_dist_inv_cdf` + the tower fixes, or whether residual
  `Fraction`/`Decimal` edges need a native `_decimal` first (in which case
  the row is rewritten measured and deferred).
- **`array` `'w'` typecode.** The 3.13 UCS4 `'w'` typecode touches the
  unicode/buffer boundary; if it proves deeper than a typecode-table entry
  it splits to a follow-up.

## Future work

- **Native `_decimal`** (libmpdec-equivalent), unblocking the
  `test_decimal` `skip` and the deepest `test_statistics` edges.
- **C-ABI binary wheel loading** (`Py_LIMITED_API`/stable-ABI wheels via
  `dlopen`) — the highest-ceiling drop-in lever (real `numpy`/`pandas`),
  for which this faithful native numeric tower is the prerequisite.
- **`pyexpat`/`_elementtree`** native accelerators for the XML cluster
  (`test_pyexpat`/`test_xml_etree`), the data-tower sibling of this wave.
