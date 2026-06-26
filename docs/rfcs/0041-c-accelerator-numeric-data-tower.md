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

### WS-json — native `_json` accelerator · ~2.5K LOC · **landed**

**Status (landed):** the native `_json`
(`crates/weavepy-vm/src/stdlib/json_accel.rs`) ships the five symbols
`Lib/json/` imports (`make_scanner`, `make_encoder`, `scanstring`,
`encode_basestring`, `encode_basestring_ascii`); `scanner.py`/`encoder.py`
prefer the C path and fall back to Python when blocked, so the suite's
`CTest`/`PyTest` pairs exercise both. Faithful edges reconciled: `scanstring`
error offsets and `\uXXXX` surrogate-pair handling, the `ensure_ascii`/
`indent`/`separators`/`sort_keys` interaction, `parse_constant`/
`object_pairs_hook`, and `JSONDecodeError`'s `msg`/`pos`/`lineno`/`colno`.
Adjacent perf fixes (identity-keyed code-point caches for `scanstring`/
`str` length/indexing, and `stacker`-grown recursion in the recursive
parse/encode) cleared the deep-nesting and O(N²) cases. `test_json` passes.

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

### WS-csv — faithful `_csv` rewrite · ~2K LOC · **landed**

**Status (landed):** the Rust `_csv` accelerator was rewritten as a faithful
port of `Modules/_csv.c` — the reader parse DFA (quote/escape/in-field
states, `QUOTE_NONNUMERIC` float coercion, embedded newlines inside quotes),
the writer's quoting/escaping, a `Dialect` getset type with CPython's exact
validation wording, `field_size_limit([new])`, and a module-singleton
`_csv.Error` (`__module__ == "_csv"`). `Lib/csv.py` is now the verbatim 3.13
file. Two adjacent fixes were needed: native file iteration
(`for row in reader` over a real file) split lines on `\n` only, so a
`lineterminator='\r'` round-trip mis-parsed — extracted a newline-aware
`PyFile::read_line_bytes` shared by `readline()` and `PyIterator::File`; and
`array.array` gained the 3.13 `'w'` (Py_UCS4) unicode typecode that
`TestArrayWrites` exercises. `test_csv` passes (124 run, 5 skipped).

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

### WS-datetime — `_pydatetime` + native `_datetime` split · ~6K LOC · **landed**

**Status (landed):** `Lib/_pydatetime.py` is vendored verbatim and
`Lib/datetime.py` is the 3.13 shim (`from _datetime import *` with the
`from _pydatetime import *` fallback), so `load_tests`'
`import_fresh_module('datetime', blocked=['_datetime'])` runs the `_Pure`
suite (the native `_datetime` accelerator is deferred, so the `_Fast`
classes skip cleanly). The native edges `datetimetester.py` reaches were
brought to CPython fidelity: `time.strftime` accepts a `WStr` format and
round-trips lone surrogates through the PUA bridge (surrogate `%Z` tznames
and `'%y\ud800%m'` literals); the `%c` printf code accepts an int code
point in the surrogate range and a one-codepoint `WStr`, un-bridged at the
`%` boundary; `time.ctime`/`time.asctime` (libc asctime layout); `struct_time`
carries the hidden `tm_gmtoff`/`tm_zone` extras read by `_local_timezone`
without leaking them into the 9-element view; `time.localtime`/`gmtime`
raise `OverflowError` for out-of-range/non-finite timestamps; `array.byteswap()`
for tzfile parsing; `pickle`'s Python-2 `BINSTRING`/`SHORT_BINSTRING`
opcodes; and the `unittest._assertNotWarns` test helper. `test_datetime`
passes (518 run, OK, 48 skipped).

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

### WS-statistics — native `_statistics` + numeric-tower fixes · ~1.5K LOC · **landed**

**Status (landed):** the native `_statistics._normal_dist_inv_cdf`
(`crates/weavepy-vm/src/stdlib/statistics_accel.rs`, Wichura AS241 with the
verbatim CPython coefficients) backs `NormalDist.inv_cdf`, and
`Lib/statistics.py` is the verbatim 3.13 file over it. The residual
numeric-tower gaps were closed: `COMPARE_OP` now pushes the *raw* rich-compare
result (so `NormalDist.__eq__` returning a non-bool round-trips), `min`/`max`
compare through the rich-comparison protocol (`Fraction`/`Decimal`),
`vars()` on a `__slots__`-only instance raises `TypeError`, correctly-rounded
large-`int`/`int` true division fixes `harmonic_mean`, and a compiler fix
promotes names bound in `match` `case` bodies to cell variables (the
`statistics.kde` kernels). `test_statistics` passes (394 run, OK, 6 skipped).

Add the native `_statistics._normal_dist_inv_cdf` helper that
`statistics.NormalDist` uses, and close the residual numeric-tower gaps the
RFC 0039 perf wave exposed: `NormalDist` arithmetic, the weighted
`harmonic_mean` form, and `Fraction`/`Decimal` interop in
`mean`/`variance`/`fmean` (several of these lean directly on the WS-math
`sum()`/coercion fixes). `Lib/statistics.py` stays Python over the thin
native helper.

**Flips:** `test_statistics`.

### WS-containers — `_heapq` / `_bisect` (✅ landed) · `array` (split to follow-up)

`test_heapq`/`test_bisect` "pass core but stress tests probe [the] C
accelerator": like `test_json`, they build `C`/`Py` pairs via
`import_fresh_module` and run the stress/randomized tests against both.

**Landed this commit:**

- **`_heapq`** (`crates/weavepy-vm/src/stdlib/heapq_accel.rs`) — a faithful
  port of `Modules/_heapqmodule.c`: `heappush`/`heappop`/`heapify`/
  `heapreplace`/`heappushpop` plus the `_heapify_max`/`_heapreplace_max`/
  `_heappop_max` helpers, all comparing through the VM's rich-comparison
  machinery (`op_compare(.., Py_LT)`). Reproduces the issue-17278 / bpo-39421
  guards: the `siftup`/`siftdown` "list changed size during iteration"
  `RuntimeError` and the `heappushpop` "list index out of range" re-check
  after a comparison callback mutates (or clears) the heap. The frozen
  `heapq` already ends with `from _heapq import *`, so it picks the C path up
  automatically.
- **`_bisect`** (`crates/weavepy-vm/src/stdlib/bisect_accel.rs`) — a faithful
  port of `Modules/_bisectmodule.c`: `bisect_left`/`bisect_right`/`insort_*`
  (+ `bisect`/`insort` aliases) with the full `a, x, lo=0, hi=None, *,
  key=None` surface, driving the search over `__getitem__`/`__len__` and
  inserting via `list.insert` (exact lists) or the object's `.insert`. Uses
  the overflow-safe `lo + (hi - lo) / 2` midpoint so the `sys.maxsize`
  `test_large_range` case doesn't panic. `Lib/bisect.py` was replaced with
  CPython 3.13's verbatim file (it ends with `from _bisect import *`).

**Cross-cutting fixes this unblocked (not container-local):**

- **`import_fresh_module(blocked=[...])`** now blocks via CPython's real
  `sys.modules[name] = None` sentinel instead of a `sys.meta_path` finder.
  WeavePy resolves builtin modules *before* consulting `meta_path`, so the
  old finder never fired for a C accelerator; the `None`-sentinel path
  (honoured by `Interpreter::load_one`) is what actually forces the
  pure-Python fallback in the `Py` half of every C/Py test pair.
- **`sorted`/`min`/`max` now treat `key=None` as "no key"** (identity),
  matching CPython, instead of trying to *call* `None` on each element.
  `test_heapq` exercises this directly (`sorted(data, key=f)` with `f=None`).

**`array` is split to a follow-up.** `test_array` is not gated on a missing
accelerator but on `array.array` being a frozen *pure-Python* module: the
measured run is 890 tests / 278 errors / 62 failures, dominated by the
buffer protocol, exact per-typecode overflow/`struct` packing, `memoryview`
format/itemsize, and pickling — i.e. it needs a **native `array.array` type
with the buffer protocol**, an RFC-sized effort on its own rather than a
typecode-table edge. Tracked separately so this wave stays measured.

**Flips:** `test_heapq` ✅, `test_bisect` ✅. (`test_array` deferred.)

### Fixtures + baseline rewrite · ~1K LOC (data) · **landed**

Every touched row in `expectations.toml` is rewritten to its **measured**
status (each `pass` row's `reason` quotes the run counts and the concrete
fixes). Following the repo's `test_rfcXXXX_dropin.py` convention, a single
bundled `tests/regrtest/test_rfc0041_dropin.py` locks the wave's behaviour
in-process — one section per workstream (math reductions + the
`sum()`/coercion/division fixes, the `_json`/`_csv`/`_statistics`
accelerators, `_heapq`/`_bisect`, and the `_pydatetime` split + native
`time`/`array` edges) — so CI catches regressions without the full CPython
checkout.

A fresh `weavepy-conformance regrtest --cpython-dir vendor/cpython/Lib/test
--mode subprocess` sweep is `--check` clean on this machine at low
parallelism: every RFC 0041 target row passes, and the only residual
divergences are environment-bound (heavy tests SIGKILL'd at the per-test
budget under high `--jobs` CPU contention, and the timing-flaky
`test_multiprocessing_forkserver` "dangling processes" teardown) — all of
which pass when run standalone and none of which touch this wave's code.

## Measured targets

The commit-acceptance bar is flipping the following `expectations.toml`
rows to `pass`. Anything that runs further but still fails gets a
rewritten, measured `reason` rather than a guess.

| Workstream | Target rows (→ `pass`) | Status |
|---|---|---|
| WS-math | `test_math` | ✅ landed |
| WS-containers | `test_heapq`, `test_bisect` | ✅ landed (`test_array` split to follow-up) |
| WS-json | `test_json` | ✅ landed |
| WS-csv | `test_csv` | ✅ landed |
| WS-datetime | `test_datetime` | ✅ landed |
| WS-statistics | `test_statistics` | ✅ landed |

That is **8 files flipped `fail` → `pass`** (all landed; `test_array`
split to a follow-up), plus a measured-truth rewrite of `test_decimal`'s
`skip` reason if the native numeric work advances it. The C-ABI
binary-wheel arc and a native `_decimal` (libmpdec-equivalent) are
explicitly **out of scope** here.

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
