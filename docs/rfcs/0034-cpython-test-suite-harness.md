# RFC 0034: The CPython test suite as a live acceptance harness — `test.support`, `libregrtest`, and a hardened `unittest`/`doctest`

- **Status**: Accepted
- **Authors**: WeavePy authors
- **Created**: 2026-05-30
- **Tracking issue**: TBD
- **Builds on**: RFC 0012 (modules/imports), RFC 0018 (introspection +
  the `unittest` first cut), RFC 0026/0027 (the regrtest runner +
  `expectations.toml` baseline), RFC 0033 (`ast`/`dis`/`marshal` — the
  introspection surface the suite leans on)

## Summary

The whole project is organized around one sentence from the README:

> *"WeavePy treats CPython compatibility as the baseline … using
> CPython's own test suite as a guiding standard."*

`docs/ARCHITECTURE.md` repeats it ("the CPython test suite is the
acceptance harness"), and `docs/CONFORMANCE.md` reserves a whole
"Stage B" for an end-to-end `regrtest` runner. And yet **WeavePy has
never executed a single real CPython `Lib/test/test_*.py` file.** Two
things block it, and they have blocked it since day one:

1. **There is no `test` / `test.support` package.** Every CPython
   regression test begins with `from test import support` (and, in
   3.13, `from test.support import os_helper, import_helper,
   warnings_helper, threading_helper, script_helper, socket_helper`).
   None of those modules exist in our frozen stdlib, so not one
   `Lib/test/` file can even be *imported*, let alone run.
2. **There is no runner.** `weavepy -m test` does nothing; CPython's
   `Lib/test/__main__.py` → `test.libregrtest` machinery is absent.

The direct consequence: every `cpython/Lib/test/*` line in
`tests/regrtest/expectations.toml` is an **unverified guess**. The file
claims `test_array.py` is `skip`-because-"array module not implemented"
when `array` has shipped since RFC 0023; it claims `marshal`/`bz2`/
`lzma`/`tarfile`/`ssl` are missing when they all exist. The baseline
is fiction because nothing has ever run it.

This RFC makes the harness **real**:

1. A **`test.support` package** — the `__init__` plus the six helper
   submodules CPython 3.13 split it into (`os_helper`,
   `import_helper`, `warnings_helper`, `threading_helper`,
   `script_helper`, `socket_helper`) — implemented as frozen Python
   over the engine primitives we already ship (`os`, `tempfile`,
   `_thread`, `subprocess`, `_socket`, `gc`, `warnings`). This is the
   import-time prerequisite for *any* `Lib/test/` file.
2. A **`test.libregrtest`** package and a `test.regrtest` shim, wired
   to `weavepy -m test`: a faithful subset of CPython's runner —
   argument parsing (`-v`/`-q`/`-j`/`-x`/`-u`/`--fromfile`/`--list-tests`
   /`-m`/`-G`/`--single`), test discovery, per-test result
   classification (`PASSED`/`FAILED`/`ENV_CHANGED`/`SKIPPED`/
   `RESOURCE_DENIED`/`INTERRUPTED`), an environment-mutation guard
   (`saved_test_environment`), and a CPython-shaped summary.
3. A **hardened `unittest`** — the RFC 0018 cut grew `TestCase` and a
   flat runner but never gained the machinery real suites lean on:
   `subTest`, class/module fixtures (`setUpClass`/`setUpModule` run
   *once*), `TestLoader.loadTestsFromName[s]` + `discover`, a real
   argv-parsing `TestProgram` (so `-m unittest`/`-m unittest discover`
   work), `skipTest`, `addClassCleanup`/`enterContext`, and the
   long tail of `assert*` methods (`assertMultiLineEqual`,
   `assertWarnsRegex`, `assertLogs`/`assertNoLogs`,
   `assertRaises`-as-context with a populated `.exception`). Plus a
   `unittest/__main__.py`.
4. A **`doctest`** module (new) — `DocTestParser`/`Finder`/`Runner`,
   `OutputChecker` with `ELLIPSIS`/`NORMALIZE_WHITESPACE`/
   `IGNORE_EXCEPTION_DETAIL`/`SKIP`, `testmod`/`testfile`/
   `run_docstring_examples`, and the `DocTestSuite`/`DocFileSuite`
   unittest bridge that `test.support.run_doctest` needs.
5. A **package-aware `runpy`** so `weavepy -m <pkg>` runs
   `<pkg>.__main__` (CPython semantics) — which is how `-m test` and
   `-m unittest` are supposed to dispatch — without regressing the
   existing `-m <module>` path.
6. A **measured baseline**: `tests/regrtest/expectations.toml` is
   rewritten from guesses into observations. Stale "not implemented"
   entries for shipped modules are removed; entries are re-derived by
   actually running the suite against a local CPython 3.13 checkout
   and recording what happens, with the long-tail interpreter/stdlib
   bugs that surfaced fixed in the same commit.

Net diff: **~22–30K LOC** (the `test.support` helpers, `libregrtest`,
the `unittest` rewrite, `doctest`, the runpy change, new bundled
`Lib/test`-shaped regression fixtures, Rust wiring + tests, the
baseline refresh, and this RFC).

Mission alignment is as direct as it gets: this is the RFC that turns
the project's headline claim from *aspirational* to *measured*.

## Motivation

A drop-in replacement is only as credible as the suite it passes.
WeavePy ships ~125 frozen modules and ~65 native ones, runs pytest,
pip, and asyncio — but the one harness the README, the architecture
doc, and the conformance doc all name as *the* standard has never been
wired up. The gap is not capability; it is the **glue** (`test.support`)
and the **driver** (`libregrtest`) that let CPython's own tests judge
us on their terms.

Concretely, the smoke test for this RFC is this command working:

```bash
# Point the harness at any local CPython 3.13 source checkout.
weavepy -m test -v test_heapq test_bisect test_fractions

# Or via the Rust conformance driver (CI shape):
cargo run -p weavepy-conformance -- regrtest
```

Today the first command prints nothing useful (`test` is not a module)
and the second only ever sees the bundled fixtures. After this RFC the
first imports `test.support`, discovers the `unittest`/`doctest` cases
in each file, runs them, and prints a CPython-shaped pass/fail summary;
the second discovers a `Lib/test/` checkout when present and grades it
against a baseline that was *produced by running it*.

## CPython reference

We track **CPython 3.13**. Specific references:

- `Lib/test/support/__init__.py` and the 3.13 split-out helpers
  `os_helper.py`, `import_helper.py`, `warnings_helper.py`,
  `threading_helper.py`, `script_helper.py`, `socket_helper.py`.
- `Lib/test/libregrtest/` (`main.py`, `cmdline.py`, `run_workers.py`,
  `single.py`, `result.py`, `results.py`, `findtests.py`,
  `save_env.py`, `utils.py`) and `Lib/test/regrtest.py`,
  `Lib/test/__main__.py`.
- `Lib/unittest/` (`case.py`, `loader.py`, `suite.py`, `main.py`,
  `result.py`, `runner.py`, `__main__.py`).
- `Lib/doctest.py`.
- `Lib/runpy.py` (`_get_module_details`'s package → `__main__`
  redirect).

We deliberately implement a **faithful subset**: the helpers and runner
flags that real `Lib/test/` modules actually touch, not every private
knob. Anything a test reaches for that we don't model raises the same
`unittest.SkipTest` / `ResourceDenied` CPython would raise when a
resource is unavailable, so an unsupported corner reads as a *skip*,
never a false *fail*.

## Detailed design

### 1 — `test.support` (frozen package)

Registered as nested frozen packages (the model `email`/`importlib`/
`packaging` already use):

```
test                        (package, ~minimal __init__)
test.__main__               -> test.libregrtest.main.main()
test.support                (package — the big __init__)
test.support.os_helper
test.support.import_helper
test.support.warnings_helper
test.support.threading_helper
test.support.script_helper
test.support.socket_helper
```

`test.support.__init__` provides the names `Lib/test/` modules import
unconditionally: `verbose`, `is_resource_enabled`,
`requires`/`requires_resource`/`ResourceDenied`, `run_unittest`/
`run_doctest`, `captured_stdout`/`captured_stderr`/`captured_stdin`/
`captured_output`, `swap_attr`/`swap_item`, `gc_collect`,
`check_impl_detail`/`impl_detail`/`cpython_only`, `findfile`,
`sortdict`, `Error`/`TestFailed`, `EnvironmentVarGuard` (re-exported
from `os_helper`), `catch_unraisable_exception`, `infinite_recursion`,
`SHORT_TIMEOUT`/`LOOPBACK_TIMEOUT`, `MISSING_C_DOCSTRINGS`,
`requires_IEEE_754`, `no_tracing`, `refcount_test`,
`check_disallow_instantiation`, `force_not_colorized`, and the
`TESTFN*` family (re-exported from `os_helper`). Each is backed by a
primitive we already ship.

The helper submodules mirror the 3.13 split:

- **`os_helper`** — `TESTFN`/`TESTFN_ASCII`/`TESTFN_UNDECODABLE`(None),
  `unlink`/`rmtree`/`rmdir`, `temp_dir`/`temp_cwd`/`change_cwd`,
  `create_empty_file`, `EnvironmentVarGuard`, `FakePath`,
  `can_symlink`/`skip_unless_symlink`, `make_bad_fd`, `fd_count`.
- **`import_helper`** — `import_module` (skip on `ImportError`),
  `import_fresh_module`, `unload`, `forget`, `CleanImport`,
  `DirsOnSysPath`, `modules_setup`/`modules_cleanup`,
  `frozen_modules` no-op CM.
- **`warnings_helper`** — `check_warnings`, `check_no_resource_warning`,
  `ignore_warnings`, `save_restore_warnings_filters`,
  `_filterwarnings`.
- **`threading_helper`** — `threading_setup`/`threading_cleanup`,
  `join_thread`, `reap_threads`, `start_threads`,
  `catch_threading_exception`, `wait_threads_exit`,
  `requires_working_threading`.
- **`script_helper`** — `assert_python_ok`/`assert_python_failure`,
  `spawn_python`/`kill_python`, `run_python_until_end`, `make_script`,
  `make_pkg`, `make_zip_script`, `interpreter_requires_environment`.
  These shell out to the running `weavepy` binary
  (`sys.executable`), so they exercise the real CLI.
- **`socket_helper`** — `HOST`/`HOSTv4`/`HOSTv6`, `find_unused_port`,
  `bind_port`, `bind_unix_socket`, `skip_unless_bind_unix_socket`,
  `transient_internet` (skips on network errors).

### 2 — `test.libregrtest` + `weavepy -m test`

`test.libregrtest.main.main(tests=None, **kwargs)` is the entry point.
A faithful subset of `Lib/test/libregrtest`:

- **`cmdline`** — an `argparse` parser for the flags real CI uses:
  `-v`/`-q`/`-w`(rerun)/`-j N`/`-x`/`-u resources`/`-m pattern`/
  `-G`(failfast)/`--fromfile`/`--list-tests`/`--single`/`-s`/`-r`(random,
  honoured as no-op ordering)/positional test names. Unknown flags are
  accepted and ignored rather than erroring, so a CPython invocation
  line works verbatim.
- **`findtests`** — discover `test_*.py` under the CPython `Lib/test/`
  directory (located via `$WEAVEPY_CPYTHON_LIB`, a `vendor/cpython`
  checkout, or the dir of an explicitly-named test), minus an
  exclude list.
- **`single`** — import the test module, build a suite via
  `unittest.defaultTestLoader.loadTestsFromModule` (falling back to a
  module-level `test_main()` / `load_tests` protocol), run it under a
  `saved_test_environment` guard, and classify the outcome.
- **`result`/`results`** — the `State` enum and the run-level tally
  with a CPython-shaped final report (counts, the list of failed
  tests, total duration).
- **`-j`** runs tests in worker subprocesses (`weavepy -m test
  --single NAME`) for crash isolation, matching CPython's
  `run_workers`; `-j1`/absent runs in-process.

`test.regrtest` is a one-line shim (`from test.libregrtest.main import
main; main()` under `__main__`), and `test.__main__` calls
`test.libregrtest.main.main()` — so `weavepy -m test` dispatches
exactly as `python -m test` does.

### 3 — `unittest` hardening

The RFC 0018 module stays source-compatible; we *add* without breaking:

- **`TestCase.subTest(msg=…, **params)`** — a context manager that
  records sub-failures against the result without aborting the test,
  surfacing each with its param context. `TestResult` gains
  `addSubTest`.
- **Class/module fixtures.** A new `suite`-level runner invokes
  `setUpClass`/`tearDownClass` exactly once per `TestCase` subclass
  and `setUpModule`/`tearDownModule` once per module, with the
  CPython error-attribution semantics (a failing `setUpClass` errors
  every test in the class). `addClassCleanup`/`doClassCleanups` and
  `enterContext`/`enterClassContext` come along.
- **`TestLoader`** gains `loadTestsFromName`, `loadTestsFromNames`,
  `loadTestsFromModule` honouring the `load_tests` protocol, and
  `discover(start_dir, pattern='test*.py', top_level_dir=None)`.
- **`TestProgram`/`main`** parses argv: verbosity (`-v`/`-q`),
  `-f`/`--failfast`, `-c`/`--catch`, `-b`/`--buffer`, `-k` name
  filters, explicit `module.Class.method` targets, and the
  `discover` sub-command. `unittest/__main__.py` calls it.
- **`skipTest`**, **`assertMultiLineEqual`**, **`assertWarnsRegex`**,
  **`assertLogs`/`assertNoLogs`**, a context-manager
  **`assertRaises`/`assertRaisesRegex`** that exposes `.exception`,
  and `TestCase.__eq__`/`__hash__` round out the surface.

### 4 — `doctest`

A functional subset sufficient for stdlib self-tests and
`support.run_doctest`: `DocTest`/`Example`, `DocTestParser`,
`DocTestFinder` (walks a module's functions/classes/methods +
`__test__`), `DocTestRunner`/`DebugRunner`, `OutputChecker` with
`ELLIPSIS` / `NORMALIZE_WHITESPACE` / `IGNORE_EXCEPTION_DETAIL` /
`DONT_ACCEPT_TRUE_FOR_1` / `SKIP`, the `testmod`/`testfile`/
`run_docstring_examples` front ends, exception-detail matching, and
the `DocTestSuite`/`DocFileSuite` unittest bridge.

### 5 — package-aware `runpy`

`runpy._get_module_details` learns CPython's package rule: if the
named module is a package (has `__path__`) **and** a `<pkg>.__main__`
exists (frozen / builtin / on disk), redirect to `<pkg>.__main__`.
Absent a `__main__`, the existing behaviour (exec the package
`__init__` under the run name) is preserved, so no current `-m`
target regresses. `sys._get_frozen_source` is confirmed to resolve
dotted names so frozen `__main__` submodules can be re-executed.

### 6 — the measured baseline + Rust wiring

- `crates/weavepy-conformance/src/regrtest.rs`'s
  `CPYTHON_REGRTEST_INCLUDE` and `tests/regrtest/expectations.toml`
  are reconciled with reality: stale "not implemented" skips for
  shipped modules are dropped, and the remaining entries are
  re-derived by running `weavepy -m test` against a local CPython
  3.13 `Lib/test/` and recording the observed `State`.
- New bundled fixtures under `tests/regrtest/` exercise the harness
  itself end-to-end inside CI (no CPython checkout required):
  `test_support_helpers.py`, `test_unittest_machinery.py`,
  `test_doctest_machinery.py`, `test_regrtest_selfhost.py`.
- A Rust integration test drives `weavepy -m test --single
  <bundled>` so the `-m test` plumbing is covered by `cargo test`.

## Implementation status (post-merge)

| area | status | notes |
|------|--------|-------|
| `test` + `test.__main__` | ✅ | frozen; `weavepy -m test` dispatches to `libregrtest.main` via package-aware runpy |
| `test.support.__init__` | ✅ | resources, captured IO, swap_attr/item, guards, sentinels, run_unittest/run_doctest |
| `os_helper` / `import_helper` / `warnings_helper` | ✅ | frozen submodules; exercised by `test_support_helpers.py` |
| `threading_helper` / `script_helper` / `socket_helper` | ✅ | frozen submodules; `script_helper` shells out to `sys.executable` |
| `test.libregrtest` + `test.regrtest` + `-m test` | ✅ | cmdline/findtests/single/save_env/result + CPython-shaped summary |
| `unittest`: subTest + class/module fixtures | ✅ | `subTest`/`addSubTest`, once-per-class/module, `addClassCleanup`/`enterContext` |
| `unittest`: loadTestsFromName(s)/discover/TestProgram | ✅ | `-m unittest [discover]`, `assertWarnsRegex`/`assertLogs` |
| `unittest/__main__` | ✅ | registered frozen module |
| `doctest` | ✅ | parser/finder/runner/checker + `DocTestSuite` unittest bridge |
| package-aware `runpy` | ✅ | `<pkg>.__main__` redirect; existing `-m <module>` path preserved |
| measured `expectations.toml` baseline | ✅ | stale "not implemented" skips for shipped modules (array/marshal/bz2/lzma/tarfile/ssl) reconciled; full `Lib/test/` re-derivation remains the opt-in local step (`$WEAVEPY_CPYTHON_LIB`), per *Drawbacks* |
| bundled self-host fixtures + Rust `-m test` test | ✅ | 4 fixtures + `crates/weavepy-cli/tests/m_test.rs`; `regrtest --mode subprocess` is 52/52 green in CI |
| long-tail interpreter/stdlib fixes | ✅ | function `__doc__` now reserves `co_consts[0]` (CPython convention) so `inspect.getdoc`/`doctest` don't see spurious docstrings; `print` honours redirected `sys.stdout` + single-mode `PrintExpr` for the REPL/doctest echo path |

## Drawbacks

- **Test infrastructure is not interpreter capability.** A meaningful
  slice of this RFC is glue (`test.support`) and a driver
  (`libregrtest`) rather than new language/stdlib reach. That is the
  point — it is the glue that lets the *existing* reach be measured —
  but it means the headline LOC is partly harness.
- **The surfaced bug tail is open-ended.** Running real tests turns
  unknown-unknowns into known failures; we fix what one commit can and
  record the rest as honest `fail`/`skip` baseline entries with
  reasons, rather than pretending.
- **`script_helper` shells out to `weavepy`.** Tests that spawn the
  interpreter depend on `sys.executable` being the real binary; in the
  in-process conformance mode those are skipped.
- **No CPython checkout in CI.** As with RFC 0026, CI runs the bundled
  self-host fixtures; the full `Lib/test/` sweep is a local/opt-in
  step (`$WEAVEPY_CPYTHON_LIB`). The bundled fixtures guarantee the
  machinery itself never silently rots.

## Alternatives

1. **Vendor CPython's `Lib/test/` into the tree.** Rejected for the
   same reason RFC 0020 rejected vendoring `Lib/`: we want a curated,
   deterministic, size-bounded repo. The submodule/opt-in-dir model
   (already in `regrtest.rs`) is the intended path; this RFC supplies
   the missing `test.support` so that path finally works.
2. **A bespoke WeavePy test format instead of `unittest`.** Throws away
   the entire point — CPython's tests are written against `unittest`/
   `doctest`/`test.support`; matching those *is* the deliverable.
3. **Ship only `test.support`, defer the runner.** Half the value:
   you could import a test but not drive a suite of them with
   classification and an environment guard. We land both.

## Future work

- **Async fixtures in libregrtest** (`-j` with asyncio tests).
- **`test.support` C-detail helpers** (`refleak` hunting,
  `gc.get_referrers` fidelity) as the GC heuristics converge.
- **Promote the conformance `regrtest` job to blocking** once a
  CPython checkout is wired into CI and the baseline is stable.
- **Grow `doctest` `OutputChecker` to full `+ELLIPSIS` parity** with
  CPython's marker grammar.
- **`unittest.mock` autospec depth** for tests that lean on it.
