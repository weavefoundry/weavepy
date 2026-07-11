# RFC 0049: CPython `Lib/test/` conformance sweep, wave 5 — full-suite discovery and the measured whole-suite baseline

- **Status**: Implemented
- **Authors**: WeavePy authors
- **Created**: 2026-07-10
- **Tracking issue**: TBD
- **Builds on**: RFC 0048 (wave 4 — verbatim `test.support` +
  application-stack modules, main-thread GIL), RFC 0038 (wave 3), RFC
  0037 (wave 2), RFC 0036 (vendored `Lib/test/` checkout + the measured
  `expectations.toml` baseline and its `--check` gate).

## Summary

Waves 1–4 graded WeavePy against a **curated allowlist** of CPython
test files (163 files in `CPYTHON_REGRTEST_INCLUDE`, plus whatever
`expectations.toml` names — 227 labels at the close of wave 4). The
other **259 files and most of the 35 test packages** in the vendored
CPython 3.13 checkout were never attempted at all — and they include
the files that define Python-the-language: `test_builtin`,
`test_types`, `test_str`, `test_long`, `test_syntax`, `test_patma`,
`test_super`, `test_scope`, the comprehension/genexp family,
`test_buffer`/`test_memoryview`, and the CLI surface
(`test_cmd_line`, `test_repl`, `test_site`).

Wave 5 retires the allowlist as a *scope* mechanism. The deliverable
is a **measured whole-suite baseline**: every `test_*.py` file and
every `test_*` package in `vendor/cpython/Lib/test/` is scheduled,
executed, and graded, and `tests/regrtest/expectations.toml` carries a
measured row (with a concrete, actionable reason) for every non-pass.
On top of the measurement, the wave closes the highest-value failure
clusters the sweep surfaces — prioritized as: (1) rows whose recorded
blockers have already been fixed by later waves ("stale reds"), (2)
the core-language cluster, (3) near-green long-tail modules where one
root cause flips several files. Everything left red carries a measured
first-failure so later waves start from evidence, not guesses.

After this wave, "WeavePy's conformance number" means one thing: the
pass rate over CPython's entire test suite, reproducible with a single
command, gated by CI against regression.

## Motivation

The README has promised since RFC 0036 that "the full allowlist is
still being worked through file by file." Wave 5 is that promise.
Three concrete problems with the curated-allowlist status quo:

1. **Unknown unknowns dominate the risk.** A drop-in-replacement claim
   is only as strong as its least-tested surface. Nobody has measured
   WeavePy against `test_builtin` or `test_syntax`; a surprise failure
   there would be far more damaging than any known-red row. Measuring
   everything converts unknown unknowns into a triaged backlog.
2. **The allowlist hides cheap wins.** Wave-4-era rows record blockers
   that later work already fixed (probe-verified: PEP 695 type-param
   syntax and PEP 646 star-unpacked annotations now parse; the
   verbatim `logging` package ships `LoggerAdapter`; verbatim
   `test.support` provides `load_package_tests`, which was the sole
   recorded blocker for `test_sqlite3` and `test_zoneinfo`). Stale
   rows cost credibility in both directions.
3. **A partial denominator is a soft metric.** "183 pass of 227
   attempted" invites the question "and the other 200 files?". The
   honest number — pass rate over the whole suite — is the only one
   worth optimizing, and the only one that can't quietly improve by
   *not* attempting hard files.

## CPython reference

- **The suite itself**: `vendor/cpython/Lib/test/` (3.13 branch) — 392
  `test_*.py` files + 35 `test_*` packages at the pinned checkout.
- **Discovery/grading semantics**: `Lib/test/libregrtest/findtests.py`
  (`findtests()` scans the whole directory; packages are discovered by
  their `__init__.py`), `Lib/test/libregrtest/main.py` (per-test
  isolation, resource gating).
- **Language-defining modules the sweep newly attempts**:
  `test_builtin`, `test_types`, `test_syntax`, `test_grammar`
  siblings (`test_global`, `test_scope`, `test_named_expressions`,
  `test_patma` for PEP 634), `test_genericalias` (PEP 585),
  `test_type_params`/`test_type_aliases` (PEP 695),
  `test_pep646_syntax` (PEP 646), `test_positional_only_arg`
  (PEP 570), `test_except_star` (PEP 654).
- **Behavioral references for expected cluster fixes**: cited per-row
  in `expectations.toml` as reasons are measured; the RFC deliberately
  does not pre-cite fixes it has not yet measured the need for.

## Detailed design

### WS-A — Harness: full-suite discovery

Two mechanical changes in `crates/weavepy-conformance`:

1. **Package discovery.** `discover_with` under
   `DiscoveryOptions::include_all_cpython` currently schedules only
   `test_*.py` *files*. It now also schedules `test_*` *directories*
   containing an `__init__.py`, under the same `<name>.py` label
   convention the curated allowlist already uses for packages
   (`cpython/Lib/test/test_asyncio.py` labels the `test_asyncio/`
   package). One label convention, whether the target is a file or a
   package, so expectations rows key identically.
2. **The expectations file becomes the source of scope.** After the
   WS-B rewrite, `expectations.toml` names every attempted label, so
   the default discovery path (curated floor ∪ expectations keys)
   covers the full suite *without* `--all-cpython`. The flag remains
   meaningful as the guard that surfaces files *added* to the vendored
   checkout that have no row yet — refreshing the baseline is
   `--all-cpython --no-check`, grading it is the default invocation.

Grading policy is unchanged (RFC 0036): `pass`/`fail`/`error`/`skip`/
`timeout` per file in crash-isolated `--mode subprocess`, SIGKILL wall
budget, `--check` exits non-zero on any divergence from the baseline.

### WS-B — The measured whole-suite baseline

One sweep, one rewrite:

- Run `weavepy-conformance regrtest --all-cpython --no-check
  --mode subprocess --jobs N` against the vendored checkout on the
  release binary.
- Rewrite `tests/regrtest/expectations.toml` from the report:
  - Every newly attempted file that passes gets **no row** (the
    default is "expect pass" — same convention as today).
  - Every non-pass gets a row whose `reason` quotes the measured
    first failure (the exception type + message, or the first failing
    subtest), not a guess.
  - Existing rows keep their (often hard-won) reasons when the verdict
    is unchanged; rows whose verdict improved are re-measured and
    rewritten; per-row `timeout_seconds` overrides are preserved and
    extended to newly attempted slow-but-stable files.
- Grading budget: the global 60s per-test cap stands. Files that are
  correct-but-slow get per-row overrides; files that hang get `skip`
  rows with the hang site recorded, never a silent omission.

The committed baseline must be `--check` clean: a fresh sweep reports
**`unexpected 0`** over the full suite.

### WS-C — Stale-red reclamation

Re-measure and close the rows whose recorded blockers no longer exist.
Probe-verified candidates going in: `test_typing` (row cites PEP 695,
which now parses), `test_inspect` (row cites PEP 646 parse error,
which now parses), `test_logging` (row cites missing `LoggerAdapter`;
the wave-4 verbatim `logging` package ships it), `test_sqlite3` and
`test_zoneinfo` (rows cite `test.support.load_package_tests`, which
the wave-4 verbatim `test.support` provides). Each either flips to a
measured `pass` or gets its *actual* current first-failure recorded
and, where the residual is small, fixed.

### WS-D — The core-language cluster

The highest-signal newly attempted set. Target: flip green (or record
a measured, narrow residual for) the files that test the language
itself rather than a library: the object model and builtins
(`test_builtin`, `test_types`, `test_bool`, `test_long`, `test_str`,
`test_hash`, `test_compare`, `test_richcmp`, `test_binop`,
`test_unary`, `test_augassign`, `test_index`, `test_pow`,
`test_slice`, `test_range`, `test_sort`, `test_property`,
`test_super`, `test_funcattrs`, `test_metaclass`,
`test_dynamicclassattribute`), syntax and scoping (`test_syntax`,
`test_scope`, `test_global`, `test_keyword`, `test_eof`,
`test_flufl`, `test_named_expressions`, `test_positional_only_arg`,
`test_pep646_syntax`, `test_type_params`, `test_type_aliases`,
`test_type_annotations`), control flow and callables (`test_extcall`,
`test_raise`, `test_baseexception`, `test_exception_hierarchy`,
`test_exception_variations`, `test_except_star`,
`test_exception_group`, `test_generator_stop`, `test_yield_from`),
comprehensions and iteration (`test_listcomps`, `test_setcomps`,
`test_dictcomps`, `test_genexps`, `test_enumerate`, `test_iterlen`,
`test_contains`), pattern matching (`test_patma`), and the container
tail (`test_dictviews`, `test_ordered_dict`, `test_deque`,
`test_defaultdict`, `test_userdict`, `test_userlist`,
`test_userstring`, `test_genericalias`, `test_genericclass`,
`test_typechecks`).

Fixes here follow the established wave discipline: root-cause in the
VM/compiler/parser, never test-specific special cases; each fix cites
the CPython source it matches; regressions guarded by the baseline.

### WS-E — Long-tail root-cause clusters

The sweep will bucket the remaining new reds by first failure. Fix in
descending order of (files unblocked ÷ effort), the same heuristic
waves 2–4 used, e.g. one missing native module or one `os` surface gap
often blocks five files. This workstream is explicitly
capacity-bounded: it stops when the wave's budget is spent, and
whatever remains is a measured row, not a TODO.

### Acceptance bar

1. Every `test_*.py` file and `test_*` package in the vendored
   checkout is scheduled and graded — no silent omissions; the
   README's "full allowlist" caveat is retired.
2. The committed `expectations.toml` is `--check` clean
   (`unexpected 0`) on a fresh full-suite sweep, and every non-pass
   row carries a measured reason.
3. The WS-C stale reds are re-measured (flipped or re-reasoned), and
   the WS-D core-language cluster is green except for rows whose
   measured residual is documented as out-of-wave.
4. No previously green row regresses.
5. `ci.yml` (fmt, clippy, workspace tests, bundled regrtest gate)
   passes.

## Drawbacks

- **Wall-clock cost of the gate.** A full-suite sweep is hours, not
  minutes, even at `-j8`; it stays a local/nightly gate (as the
  vendored checkout already is), while CI keeps gating the bundled
  suite. The baseline file is how full-suite regressions surface in
  review.
- **A lower headline pass-rate.** Attempting 427 labels instead of 227
  necessarily reports more red. That is the point, but the README
  status must present it honestly (measured denominator, not a dip).
- **Baseline churn.** Whole-suite rows mean bigger `expectations.toml`
  diffs in future waves. Mitigated by the no-row-for-pass convention.

## Alternatives

- **Keep growing the curated allowlist file-by-file.** Rejected: four
  waves in, the marginal cost of curation now exceeds the cost of
  measuring everything, and the unmeasured set is exactly where the
  drop-in risk lives.
- **Skip the measurement and jump straight to a domain wave (e.g.
  asyncio).** Rejected: without the full baseline, wave sequencing
  keeps being guesswork; the measurement makes every later wave
  cheaper to scope, including asyncio.
- **Gate CI on the full sweep.** Rejected for now: hours-long CI is a
  worse regression-detection loop than a check-clean committed
  baseline refreshed with each wave.

## Prior art

- **CPython `libregrtest`** discovers the whole directory and lets
  resource gates/skips — not an allowlist — narrow execution; WS-A
  converges on that model.
- **PyPy** publishes whole-suite compatibility results against its
  `lib-python` checkout; the whole-denominator number is what its
  users quote.
- **GraalPy** maintains per-file "tagged" test inventories over the
  full suite — the same measured-row-per-file shape as
  `expectations.toml`.

## Unresolved questions

- Some newly attempted files are host/platform-sensitive
  (`test_ioctl`, `test_pty`, `test_tty`, `test_curses`,
  `test_syslog`, `test_grp`/`test_pwd`) or Windows-only
  (`test_winreg`, `test_winapi`, `test_winsound`, `test_msvcrt`,
  `test_startfile`, `test_launcher`, `test_wmi`). The baseline is
  measured on macOS (the development host); rows should record
  platform-conditional verdicts the way `test_multiprocessing_fork`
  already does. A Linux re-measure is follow-up work.
- `test_asyncio` (31 submodules) will likely stay a package-level row
  with sandbox constraints noted; whether to split it into per-submodule
  synthetic labels is deferred to the asyncio wave.
- Whether `--all-cpython` should become the hard default (rather than
  expectations-driven scope) once the baseline stabilizes.

## Future work

- The asyncio wave (RFC 0048 future work), now scoped by measured
  rows instead of a package-level skip.
- A Linux/CI re-measure of the platform-sensitive rows.
- The interpreter-throughput arc: the sweep's duration column is a
  free profiling corpus; the slowest correct rows (documented 10–100×
  CPython deficits) feed the next performance RFC.
- Wave 6 of this sweep: the long-tail reds that survive WS-E.

## Implementation results (measured)

Discovery now schedules **504 labels** (392 vendored `test_*.py` files +
35 `test_*/` packages + 77 bundled fixtures), up from 227 at the close
of wave 4. The final `--check`-clean verification sweep (exit 0,
`unexpected 0`) grades:

- **All 77 bundled fixtures pass.**
- Over the **427 vendored-CPython labels**: **226 pass / 178 fail /
  9 timeout / 14 skip** — every non-pass row in
  `tests/regrtest/expectations.toml` carries a measured first-failure
  summary. Twenty rows that sat on the 60 s timeout boundary (verdict
  flipping between pass/fail/timeout with worker load) were stabilized
  with per-row `timeout_seconds` budgets sized ~3× their observed wall
  time, so the recorded verdict is the load-independent one.

Fix clusters closed while establishing the baseline (each verified by
flipping its file(s) green):

- **`SETUP_ANNOTATIONS`** — new opcode; module/class bodies containing
  annotated statements bind `__annotations__` at block entry
  (create-if-absent), `type.__annotations__` lazily creates on heap
  types and refuses static types, `module.__annotations__` lazily
  creates (CPython `type_get_annotations`/`module_get_annotations`).
  Flips `test_grammar` (import), `test_module`, `test_opcodes`,
  `test_type_annotations`, `test_pep646_syntax`; `.pyc` cache tag
  bumped to `weavepy-313-3`.
- **rich-compare `__ne__` derivation** — `object.__ne__` no longer
  re-derives from `__eq__` when the operand's own `__ne__` slot already
  ran (call-order divergence in `test_compare`).
- **bool semantics** — `bool()` strict single-argument arity;
  `__bool__` must return exact bool (`slot_nb_bool`); `__len__` results
  validated like `PyObject_Size` with `len()`-identical error strings
  (`test_sane_len`); `complex` vs `bool` numeric equality;
  `True.real`/`.numerator` demote to int; `int.__new__(bool, …)`
  rejected via the `tp_new_wrapper` "staticbase" safety check. Flips
  `test_bool`.
- **str surface** — C-argument-clinic arity on ~30 native `str`
  methods, `str.format_map` over the full mapping protocol,
  `expandtabs` negative-tabsize clamp; int shift semantics
  (`0 << huge`, saturating `>> huge`, `OverflowError` on oversized
  `<<`), `int.__itemsize__`/`__basicsize__`.
- **recursive-repr guards** — `Py_ReprEnter` equivalents on dict views
  (`...`), native `_io` stream reprs (parked `RuntimeError`), and the
  `_pyio` `FileIO`/`Buffered*`/`TextIOWrapper` reprs; fixes two
  hard **native stack overflows** (`test_dictviews`, `test_fileio`).
- **code-object value equality** — CPython `code_richcompare`
  semantics (name/arity/bytecode/consts recursively; line tables and
  inline caches excluded); `co_firstlineno == 1` for module code.
- **`codeop`** — CPython-shaped `_maybe_compile` with an
  incomplete-input classifier over WeavePy's parser messages
  (`PyCF_DONT_IMPLY_DEDENT` interactive suite rule included);
  `test_codeop` down from wholesale ImportError to a single residual
  (dual `SyntaxWarning` on `'\e' is 0`).
- **verbatim adoptions** — CPython `configparser` replaces the 440-line
  shim; `codecs` gains the six built-in error handlers as callables
  (`backslashreplace_errors` & co.);
  `importlib._bootstrap_external._get_sourcefile` unblocks
  `test_import`/`test_types` imports.

Residual clusters are recorded as measured rows for wave 6; the
largest are the CLI/subprocess family (`test_cmd_line*`, env-var
handling panic), the codec encodings family (CJK codecs unimplemented),
and the slow-timeout tail (`test_unittest`, `test_weakref`,
`test_zipimport` under parallel load).
