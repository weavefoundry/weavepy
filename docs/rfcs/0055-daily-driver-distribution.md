# RFC 0055: The daily-driver wave — a truthful distribution surface (`sysconfig`/`venv`/`ensurepip`/`runpy`/`zipimport`), CLI/REPL fidelity, and a measured ecosystem harness

- **Status**: Draft
- **Authors**: WeavePy authors
- **Created**: 2026-07-19
- **Tracking issue**: TBD
- **Builds on**: RFC 0053 (wave 8 — materialized `Lib/` tree, truthful
  `sys.prefix`, verbatim `site`/`py_compile`/`pydoc`; its future-work
  section names the `zipimport` arc), RFC 0030 (pure-Python `pip`
  (`_minipip`) + PyPI resolver this wave promotes to the bootstrap
  path), RFC 0040 WS5 (the self-contained `zipimport` this wave makes
  fast and faithful), RFC 0033 (`.pyc` read/write + `cpython_code`
  codec the `__cached__` story rides on), RFC 0020 (drop-in CLI/REPL),
  RFC 0049 (measured whole-suite baseline protocol).

## Summary

WeavePy can already create a venv and `pip install six` into it — the
plumbing landed across RFCs 0020/0030/0053. What it cannot yet do is
*be somebody's `python3`*: the distribution surface that every
installer, build frontend, IDE, and CI system interrogates is still a
collection of WeavePy-shaped approximations, and the measured rows
say so precisely:

- `test_sysconfig` dies at import: `ModuleNotFoundError: _sysconfig`
  (CPython 3.13's native build-info module), and the frozen
  `sysconfig` diverges from the vendored file by exactly the
  `osx_framework_library` scheme block.
- `test_platform` is 30 errors from one line: `sys._git` does not
  exist (`sys._framework` is also absent).
- `test_venv` (12E/16F) fails on the shim `venv`'s missing CLI
  surface (`--without-scm-ignore-files`, `--copies`, `--prompt`,
  `upgrade_deps`), missing activation-script fidelity
  (`deactivate` under `set -eu`, special-chars quoting, csh), and
  `sysconfig.get_config_var('_base_executable')`.
- `test_ensurepip` is 32 errors from shape alone: the tests
  `mock.patch("ensurepip._run_pip")` — our 129-line shim has no
  `_run_pip`, no `_bundled` wheel, no `_uninstall` helper.
- `test_runpy` fails 10 cases on `__cached__`/`.pyc` semantics and
  `alter_sys` bookkeeping our 357-line rewrite approximates; CPython's
  `runpy.py` is 319 lines and imports nothing we lack any more.
- `test_zipimport` runs 83s against a 60s budget (every archive
  re-parsed through pure-Python `zipfile`) and carries an E/F cluster;
  `test_zipimport_support`, the zip legs of `test_runpy`, and the
  `test_cmd_line_script` timeout sit on the same substrate.
- `test_cmd_line` (31 residuals: `-X` matrix, `-W` plumbing,
  stdin/tty), `test_repl` (traceback/linecache fidelity, asyncio
  REPL), `test_pkg` (parent-package attribute binding after submodule
  import) are the remaining "I typed `python` and it behaved wrong"
  rows.

Wave 10 makes the distribution surface *truthful* — native
`_sysconfig`, verbatim `sysconfig`/`venv`/`ensurepip`/`runpy`, a
`zipimport` that is fast and measured, CLI/REPL fidelity — and then
*proves* the daily-driver claim with a new measured harness: the
`weavepy-conformance ecosystem` subcommand creates a venv, installs
real PyPI packages (`six`, `attrs`, `click`, `jinja2`, `requests`,
`python-dateutil`, …), runs functional smoke probes and selected
upstream test suites, and grades the result against a checked-in
`tests/ecosystem/expectations.toml` — the same
measured-baseline discipline `Lib/test` gets, applied to the
ecosystem the README promises.

As with every wave since RFC 0036, the deliverable is measured: the
full sweep re-runs, every touched row in
`tests/regrtest/expectations.toml` is rewritten from evidence (many
of the target rows still carry stale wave-5 first-failure text), and
every remaining red carries an actionable first-failure reason.

## Motivation

1. **"Drop-in replacement" is judged in the shell, not the test
   runner.** The first thing every real user does is
   `python -m venv .venv && pip install -r requirements.txt`. The
   loop mechanically works today, but every tool that *inspects* the
   environment — pip's own `sysconfig`-driven scheme resolution,
   `virtualenv`, poetry/uv probing `sys._base_executable`,
   setuptools reading `get_config_var("EXT_SUFFIX")`, CI images
   calling `platform.python_build()` — hits the untruthful edges the
   measured rows document. Each approximation is a support ticket.
2. **The rows are shape, not semantics.** `test_ensurepip`'s 32
   errors are one missing function name; `test_platform`'s 30 errors
   are one missing `sys` attribute; `test_sysconfig` dies on a
   module CPython generates mechanically at build time. This is the
   cheapest red cluster on the board per flipped label — but only if
   we adopt CPython's files verbatim instead of growing the shims
   (the RFC 0048 lesson, re-learned every wave since).
3. **`zipimport` gates three labels and a real feature.** Zip-based
   deployment (`python app.pyz`, PEP 441 `zipapp`) is a first-class
   CPython feature. Our pure-Python reimplementation is functionally
   present (RFC 0040) but re-parses archives per lookup — 83s where
   CPython takes seconds — and misses `.pyc`-in-zip, negative-offset
   archives (self-extracting), and `invalidate_caches`. The
   `test_cmd_line_script` timeout (zip-script legs) and
   `test_zipimport_support` (doctests inside archives) are the same
   root.
4. **The CLI is the product.** `test_cmd_line`'s residuals (`-X`
   options, `-W` → `warnings.filters` plumbing, stdin execution,
   `-i`) and `test_repl`'s (interactive frames in `linecache`,
   correct SyntaxError line reporting, `python -m asyncio`) are the
   exact behaviors a human notices in the first five minutes.
5. **Nothing today proves a real package works.** The regrtest sweep
   grades CPython's suite; no gate grades *PyPI*. RFC 0029 proved a
   binary wheel installs mechanically; RFC 0030 built pip — but
   "requests works" is still folklore, not a row. A measured
   ecosystem baseline converts the README's central claim into CI
   evidence, and every failure it surfaces is a real-world
   conformance bug with a reproducer attached.
6. **Cost of inaction.** The conformance long tail elsewhere
   (numerics edges, re engine internals) does not block adoption;
   a `venv` that tools cannot introspect does. Leaving this cluster
   red keeps WeavePy a test-suite artifact rather than a runtime
   people point their shebang at.

## CPython reference

- `Modules/_sysconfig.c` — `_sysconfig.config_vars()`, the native
  build-info dict `sysconfig` merges at import (3.13's split of
  build-time vars out of `_sysconfigdata`).
- `Lib/sysconfig/__init__.py` — adopted verbatim; the
  `osx_framework_library` scheme and `_get_preferred_schemes`'
  `sys._framework` dispatch are the current 19-line delta.
- `Python/sysmodule.c` — `sys._git` (3-tuple), `sys._framework`
  (empty string on non-framework builds), `sys._base_executable`
  handling.
- `Lib/venv/__init__.py`, `Lib/venv/__main__.py`,
  `Lib/venv/scripts/{common,posix}/*` — adopted verbatim, including
  the `activate`/`activate.csh`/`activate.fish` scripts and the
  `.gitignore` (`scm_ignore_files`) machinery (gh-83417).
- `Lib/ensurepip/__init__.py`, `__main__.py`, `_uninstall.py` —
  adopted verbatim; `_bundled/pip-*.whl` becomes a WeavePy-built
  wheel of the RFC 0030 pip facade (`_PIP_VERSION` matched to it).
- `Lib/runpy.py` — adopted verbatim (PEP 338);
  `importlib._bootstrap_external` spec/`cached` semantics feed its
  `__cached__` assertions.
- `Lib/zipimport.py` + `Modules/zipimport` history — the public
  surface (`zipimporter`, `_zip_directory_cache`, PEP 451 methods,
  `invalidate_caches`, `.pyc` handling with `check_hash_based_pycs`)
  our self-contained implementation must match; the
  `_frozen_importlib_external` private API it is written against is
  *not* adopted (see Alternatives).
- `Python/initconfig.c` / `Lib/test/test_cmd_line.py` — the `-X`
  option table (`faulthandler`, `dev`, `utf8`, `pycache_prefix`,
  `int_max_str_digits`, `importtime`, `tracemalloc`), `-W` →
  `sys.warnoptions` → `warnings._processoptions`.
- Acceptance tests: `Lib/test/test_sysconfig.py`, `test_platform.py`,
  `test__osx_support.py`, `test_venv.py`, `test_ensurepip.py`,
  `test_runpy.py`, `test_pkg.py`, `test_zipimport.py`,
  `test_zipimport_support.py`, `test_cmd_line.py`,
  `test_cmd_line_script.py`, `test_repl.py`, `test_site.py`,
  `test_sysconfig.py`, `test_pydoc.py` (residuals), `test_trace.py`,
  `test_support.py`, `test_regrtest.py`.

## Detailed design

### WS1 — truthful build identity: native `_sysconfig`, `sys._git`, verbatim `sysconfig`

- **`_sysconfig` (Rust, `weavepy-vm`)**: a native module exporting
  `config_vars()` — the build-time dict CPython generates in
  `Modules/_sysconfig.c` (`EXT_SUFFIX`, `SOABI`, `abiflags`,
  `Py_DEBUG`, `Py_GIL_DISABLED: 0`, `WITH_PYMALLOC`, platform
  triplet). Values come from `env!`/`cfg!` at build time and the
  existing `_weave_sysconfigdata` module (which stays, as CPython's
  `_sysconfigdata_*` analog).
- **`sys._git`**: `("WeavePy", "", "")` unless the build embeds tag
  and revision (a `build.rs` `git describe` best-effort, matching
  CPython's `--with-build-details` behavior of empty strings when
  unavailable). **`sys._framework`**: `""` (WeavePy is never a macOS
  framework build). **`sys._base_executable`**: already present via
  RFC 0053's venv work; `sysconfig.get_config_var("_base_executable")`
  starts reporting it.
- **`sysconfig` verbatim**: delete the 19-line delta — the
  `osx_framework_library` scheme block and the
  `_get_preferred_schemes` framework dispatch land as-is; with
  `sys._framework == ""` they are inert on WeavePy, which is exactly
  CPython's non-framework behavior.
- `platform` re-measured after `sys._git` (its 30-error cluster is
  that one attribute); `_osx_support` residuals (3 errors) fixed
  in-wave if shallow, enumerated if not.

### WS2 — verbatim `venv` + `ensurepip` over a real bundled wheel

- **`venv` becomes CPython's package**: `__init__.py` (687 lines),
  `__main__.py`, and the `scripts/` tree (posix `activate`,
  `activate.csh`, `activate.fish`, common `Activate.ps1`) adopted
  verbatim. The `FrozenSource` table grows a `FrozenData` sibling
  (name → `include_bytes!`) so non-`.py` resources materialize into
  the RFC 0053 `Lib/` tree; `venv.EnvBuilder.setup_scripts` reads
  them from `__file__`-relative paths exactly as upstream.
- **`ensurepip` becomes CPython's package**: `__init__.py`,
  `__main__.py`, `_uninstall.py` verbatim. `_bundled/` carries
  `pip-{ver}+weavepy-py3-none-any.whl` — a wheel *built at compile
  time by `build.rs`* zipping the frozen RFC 0030 pip facade
  (`pip/` package + generated `*.dist-info`). `_PIP_VERSION` matches
  the wheel name, so `ensurepip`'s consistency checks and
  `mock.patch("ensurepip._run_pip")`-based tests see CPython's exact
  shape. `_run_pip` runs `[sys.executable, "-W", "ignore::DeprecationWarning",
  "-c", …]` in a subprocess per upstream.
- **venv CLI surface**: with verbatim `venv/__main__.py`, the full
  argparse matrix (`--copies`, `--clear`, `--upgrade`,
  `--upgrade-deps`, `--prompt`, `--without-scm-ignore-files`) exists
  by construction. Engine gaps it exposes (e.g. `os.symlink` edge
  semantics, `subprocess` quoting in activation tests, `shutil`
  copymode) are fixed as WS6 salt.
- `pyvenv.cfg` keys follow upstream (`home`, `executable`,
  `command`, `include-system-site-packages`, `version`); the
  `implementation = WeavePy` extra key is dropped (CPython does not
  write one; tools choke on unknown keys less than on missing ones,
  but verbatim is verbatim).

### WS3 — module execution: verbatim `runpy`, `test_pkg` import semantics, fast faithful `zipimport`

- **`runpy` verbatim** (319 lines). Prerequisites it asserts:
  `spec.cached`/`__cached__` truthful for source modules (RFC 0033's
  `__pycache__` machinery wired through `SourceFileLoader.get_code`
  on the run path — `.pyc` written on first `-m` run, `__cached__`
  pointing at it), `io.open_code`, `pkgutil.get_importer`. The
  WeavePy-specific frozen-source branch
  (`sys._get_frozen_source`) is no longer needed: the materialized
  tree (RFC 0053) gives every stdlib module a real file, so
  CPython's own loader path just works.
- **`test_pkg` semantics**: after `import t2.sub.subsub`, the parent
  package's namespace must contain `sub` (attribute binding on the
  parent module object at child-import completion — CPython's
  `_handle_fromlist`/import-system contract), and `from t4 import *`
  must not import submodules not named in `__all__`. Both are fixes
  in the native import loader (`load_one`'s parent-binding and
  fromlist handling), each with a bundled regrtest.
- **`zipimport`, fast and measured**: keep the RFC 0040
  self-contained implementation (adopting CPython's file verbatim
  would require shipping `_frozen_importlib_external`'s private API —
  rejected, see Alternatives) but close the measured gaps:
  - Cache the central directory per archive in
    `_zip_directory_cache` keyed by `(path, mtime, size)` and reuse
    across `zipimporter` instances (today: re-parse per instance —
    the 83s).
  - Read the name table with a single native `zlib`-backed pass
    instead of per-entry `zipfile.ZipInfo` object construction.
  - `.pyc` entries: accept CPython-magic pycs via the RFC 0033
    `marshal`/`cpython_code` codec, honoring
    `_imp.check_hash_based_pycs`.
  - Negative-offset archives (data prepended, e.g. self-extracting
    or `zipapp` with shebang), `invalidate_caches()`,
    `get_data`/`get_source`/`get_filename` error shapes,
    `zipimporter.load_module` DeprecationWarning.
  - `__file__` inside archives: `{archive}{sep}{subpath}` exactly.
- The zip legs of `test_runpy`, `test_cmd_line_script` (its timeout
  reproduces on the zip-script cases), and `test_zipimport_support`
  ride the same substrate and are re-measured after.

### WS4 — CLI/REPL fidelity

- **`-X` option table**: parse and honor `dev` (enables warnings +
  faulthandler + asyncio debug), `utf8`, `pycache_prefix=PATH`
  (→ `sys.pycache_prefix`, respected by the RFC 0033 writer),
  `int_max_str_digits=N`, `faulthandler`, `importtime` (best-effort
  timing to stderr), `tracemalloc[=N]`; unknown `-X` keys land in
  `sys._xoptions` as CPython does (dict of str → str|True).
- **`-W` plumbing**: collect into `sys.warnoptions`, apply via
  `warnings._processoptions` at startup (after `site`), matching
  precedence with `PYTHONWARNINGS`.
- **stdin/tty**: `weavepy -` reads the program from stdin;
  `weavepy -i script.py` drops into the REPL with the script's
  globals; `PYTHONSTARTUP` errors report tracebacks without killing
  the session; isatty-dependent prompt behavior matches.
- **REPL fidelity** (`test_repl`): interactive input registered in
  `linecache` under CPython 3.13's `<python-input-N>` naming so
  tracebacks from the REPL show source lines; SyntaxError reporting
  points at the correct line for multi-line constructs;
  `python -m asyncio` starts the asyncio REPL (frozen
  `asyncio.__main__` over the RFC 0054 native `_asyncio`), including
  top-level `await` via `compile(..., PyCF_ALLOW_TOP_LEVEL_AWAIT)`
  (flag exists since RFC 0052) and contextvar persistence across
  lines.
- **Diagnose the `test_cmd_line` mid-suite crash** (the suite dies
  with a raw traceback after ~16 cases) and the
  `test_cmd_line_script` hang; fix root causes rather than budgets.
- **Principled skips**: `test_embed` and `test_getpath` exercise
  CPython's embedding artifacts (`_testembed` binary,
  `Modules/getpath.py` source file). They graduate from misleading
  `fail` rows to `skip` rows with honest reasons — same policy as
  the tkinter family.

### WS5 — the ecosystem harness: `weavepy-conformance ecosystem`

A new subcommand + two checked-in files:

- **`tests/ecosystem/manifest.toml`** — the package matrix. Each
  entry declares the pip requirement set, a *smoke probe* (inline
  Python asserting real behavior, not just import), and optionally
  an *upstream-tests* spec (how to run a subset of the package's own
  suite). Wave-1 matrix, chosen for pure-Python reach and real-world
  weight:

  | Package | Probe |
  |---|---|
  | `six` | `six.moves`, `add_metaclass` round-trip |
  | `attrs` | define/validate/asdict/frozen classes |
  | `click` | CLI invocation via `CliRunner` |
  | `jinja2` (+`markupsafe`) | template render incl. autoescape |
  | `requests` (+`urllib3`, `idna`, `charset_normalizer`, `certifi`) | GET against a local `http.server`, HTTPS against a local TLS server (rustls `_ssl`) |
  | `python-dateutil` | `parser.parse`, `rrule` expansion |
  | `typing_extensions` | `Protocol`/`TypedDict` runtime checks |
  | `packaging` | version/specifier/tags round-trips |
  | `pytest` project | run a small in-tree fixture project's tests through installed pytest |

- **Runner semantics** (Rust, `weavepy-conformance/src/ecosystem.rs`):
  per row — create a scratch venv with the CLI under test, install
  the requirement set (`--offline` mode consumes a wheel cache
  directory populated by `tools/ecosystem_fetch.py`, so the gate can
  run without network; online mode hits PyPI), run the probe in a
  subprocess with a wall budget, grade `pass`/`fail`/`skip` against
  **`tests/ecosystem/expectations.toml`**, and write
  `ecosystem.md`/`ecosystem.json` reports next to the regrtest ones.
- **CI**: a non-blocking job at first (network + PyPI drift), local
  `--check` gate for development; graduates to blocking once a
  cached-wheel offline lane is proven stable.
- The harness is how this wave's headline claim is *stated*: the
  baseline file says exactly which packages work, and a red row with
  a measured reason is the wave-11 worklist.

### WS6 — long-tail salt

Fixed alongside, each with a bundled regrtest where the fix is an
engine behavior:

- `spec.cached`/`__cached__` truthfulness for disk modules (WS3
  prerequisite; also moves `test_runpy`'s first failure).
- Parent-package attribute binding + `from pkg import *` submodule
  rules (`test_pkg`).
- `test.libregrtest.result.TestStats` (the frozen mini-libregrtest
  gains the class `test_regrtest` imports first).
- `test_support`'s first failure (`EOFError: EOF when reading a
  line` — a `captured_stdin` interaction) and `test_trace`'s
  process-return-code case, if shallow.
- Whatever the verbatim adoptions expose that fits the budget;
  larger finds land as measured rows with actionable reasons.

### WS7 — re-measure and re-baseline

Per the RFC 0049 protocol: two full sweeps
(`regrtest --all-cpython --mode subprocess --jobs 8`), cross-checked;
every row this wave touched rewritten from evidence (several still
carry wave-5 reason text that no longer matches the tree — e.g.
`test_cprofile`'s row says `ModuleNotFoundError: _lsprof` while
`_lsprof` shipped in RFC 0053); the ecosystem baseline committed with
every row measured. New bundled regrtests: `_sysconfig` surface,
venv-creation invariants (pyvenv.cfg shape, activation script
presence, `--clear`/`--copies`), ensurepip bootstrap into a scratch
prefix (offline, bundled wheel), runpy `__cached__` round-trip,
zipimport archive matrix (pyc-in-zip, prefix offset, cache
invalidation), `-X`/`-W` option plumbing, and an end-to-end
daily-driver fixture: venv → ensurepip → `pip install` a local wheel
→ run its console script.

### Acceptance criteria

1. `import _sysconfig` works; `sysconfig/__init__.py` is
   byte-identical to `vendor/cpython/Lib/sysconfig/__init__.py`;
   `test_sysconfig` moves past its import-time first failure
   (measured, residuals enumerated).
2. `sys._git`, `sys._framework` exist with CPython shapes;
   `test_platform` flips or its residuals are enumerated and small.
3. `venv/` and `ensurepip/` are verbatim CPython packages;
   `weavepy -m venv --copies --prompt demo .v && .v/bin/pip --version`
   works offline via the bundled wheel; `test_venv` and
   `test_ensurepip` move past their measured first failures with
   residuals enumerated.
4. `runpy.py` is byte-identical to CPython's; `test_runpy` flips or
   only zip-unrelated residuals remain enumerated.
5. `test_zipimport` completes inside a 60s budget with a measured
   verdict (no `timeout`); `test_cmd_line_script` reaches a measured
   verdict; `test_pkg` flips.
6. `test_cmd_line`'s residual count drops below 10 with the
   mid-suite crash gone; `test_repl` residuals enumerated at < 5.
7. `test_embed`/`test_getpath` re-classified as principled skips
   with honest reasons.
8. The ecosystem harness runs `--offline` from a wheel cache; the
   wave-1 manifest rows are all measured; at least `six`, `attrs`,
   `click`, `jinja2`, `python-dateutil`, `typing_extensions`,
   `packaging` grade `pass`.
9. At least 8 net labels flip red→green on the full sweep versus the
   wave-9 baseline, with no regressions.
10. `cargo fmt` / `clippy -D warnings` / `cargo test --workspace` /
    `regrtest --check` all green.

## Drawbacks

- **A bundled wheel in the binary.** The `build.rs`-built pip wheel
  adds ~150 KiB and a build step. Accepted: CPython ships a 2 MiB
  pip wheel in every distribution; ours is smaller because the pip
  facade is already frozen — the wheel is just its zip projection,
  generated from the same sources so it cannot drift.
- **Verbatim `venv`/`ensurepip` may expose subprocess/quoting
  gaps.** That is partly the point; each is a real conformance bug.
  WS6 reserves budget; anything too large lands as a measured row.
- **The ecosystem gate depends on PyPI.** Mitigated by the offline
  wheel-cache lane; the online lane is non-blocking in CI. Version
  drift is pinned in the manifest (exact `==` requirements, bumped
  deliberately).
- **`zipimport` stays a reimplementation.** Divergence risk against
  CPython's private-API version persists; mitigated by grading
  against `test_zipimport` itself (598 lines of behavioral spec)
  rather than code identity.

## Alternatives

- **Adopt CPython's `zipimport.py` verbatim** by also shipping
  `_frozen_importlib_external`: rejected — that private module is
  ~1,800 lines whose *other* consumers (the entire import system)
  would then exist in two implementations (the Rust loader and the
  frozen Python one), the exact dual-truth drift RFC 0053 removed.
  The public-surface reimplementation graded by the upstream suite
  is the same trade RFC 0035 made for `_sre` before the verbatim
  port became feasible.
- **Ship real upstream pip as the bundled wheel** (like CPython):
  deferred, not rejected — upstream pip imports `sqlite3`-backed
  caches, `ssl` edge surface, and ~500 files; the RFC 0030 facade
  covers the documented CLI and, per RFC 0030's design, upstream pip
  becomes installable *by* the facade the moment it fully runs. The
  bundled wheel's job is bootstrap, not feature parity.
- **Skip the ecosystem harness; rely on regrtest percentages**:
  rejected — the README's claim is about running real code, no
  `Lib/test` row proves `requests` works, and the harness's
  marginal cost is small because it reuses the venv/pip surface this
  wave hardens anyway.
- **Grow the venv/ensurepip shims instead of adopting verbatim**:
  rejected by standing policy (RFC 0048): both shims are now the
  measured first failure of their suites, and every hand-port
  re-diverges at the next CPython point release.

## Prior art

- **CPython** generates `_sysconfigdata` at build time and, since
  3.13, splits immutable build info into native `_sysconfig`
  (gh-103480) — the exact split WS1 mirrors with `build.rs`/`env!`.
- **PyPy** ships verbatim `venv`/`ensurepip` with a PyPy-built
  bundled wheel and passes these suites under its own prefix naming
  — precedent for both WS2 decisions.
- **uv and virtualenv** interrogate `sysconfig` schemes and
  `sys._base_executable` rather than trusting `sys.prefix` — the
  tools WS1's truthfulness is for.
- **RFC 0027/0036/0049** established the measured-baseline protocol
  this wave extends to a second corpus (PyPI packages); the
  expectations-file mechanics are reused wholesale.

## Unresolved questions

- Whether `test_venv`'s subprocess-heavy cases (activation under
  real bash/csh) pass inside the sandbox CI runners or need the
  suite's own `skip_unless` guards recorded — measured on the
  sweep either way.
- Whether the `requests` HTTPS probe uses a local rustls server or
  pins a public endpoint; local is hermetic and preferred, but
  exercises less of `certifi`.
- The exact `_PIP_VERSION` string: tracking the facade's own
  version (`24.0.0+weavepy`) vs adopting the upstream version it
  emulates. The wheel name and `_PIP_VERSION` must agree; the
  choice is cosmetic beyond that.
- Whether `test_cmd_line_script`'s hang has a second root cause
  beyond the zip-script legs (measured mid-wave).

## Future work

- **Real upstream pip as the bundled wheel** once the facade can
  fully self-host it (`pip install pip==25.x` then re-bundle).
- **`zipapp` + PEP 441 end-to-end** (ship `weavepy -m zipapp`
  fixtures) once zipimport is fast.
- **Ecosystem harness wave 2**: binary-wheel rows (numpy via the
  RFC 0046 ABI, `pydantic-core`, `charset_normalizer`'s mypyc
  build), and `pytest`-running-upstream-suites rows (flask, httpx).
- **Windows lane** for venv/activation (`Scripts/`, `.bat`/`.ps1`)
  and the Proactor asyncio loop RFC 0054 deferred.
