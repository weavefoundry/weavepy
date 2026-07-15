# RFC 0053: Conformance wave 8 — source-truth stdlib: a materialized `Lib/` tree, real module identity, and the verbatim tooling stack (`linecache`/`inspect`/`doctest`/`pdb`/`cProfile`)

- **Status**: Accepted
- **Authors**: WeavePy authors
- **Created**: 2026-07-14
- **Tracking issue**: TBD
- **Builds on**: RFC 0052 (wave 7 — compiler front-end fidelity; its
  future-work section names the doctest/unittest and coverage arcs
  this wave delivers), RFC 0033 (real `.pyc` read/write + `cpython_code`
  codec), RFC 0021 (frozen-code fast path this wave must preserve),
  RFC 0049 (measured whole-suite baseline protocol).

## Summary

WeavePy's stdlib is 459 modules embedded with `include_str!` and
executed with the pseudo-filename `<frozen argparse>`. That one
design decision is the measured first failure (or a residual) in a
dozen suites: anything that calls `open(module.__file__)`,
`inspect.getsource`, `linecache.getline`, or
`os.path.dirname(module.__file__)` gets a path that does not exist.
`test_argparse` is at 1022/1024 with **both** residuals traced to it;
`test_linecache` is at 26/28 for the same reason; the doctest-driven
suites (`test_cmd`, `test_genexps`, `test_metaclass`,
`test_pep646_syntax`, `test_doctest`) and `test.support`'s own path
math (`TEST_SUPPORT_DIR = dirname(abspath(__file__))`) sit on top of
it.

Wave 8 makes the stdlib *real*:

1. **A materialized `Lib/` tree.** At startup WeavePy guarantees an
   on-disk mirror of the embedded stdlib exists — found next to the
   executable (`lib/weavepy3.13/os.py` landmark, CPython-getpath
   style) or extracted once into a per-build cache directory — and
   every frozen module's `__file__` and `co_filename` point into it.
   The embedded sources remain the *execution* source of truth (the
   RFC 0021 frozen-code fast path is preserved); the disk tree is a
   byte-identical projection, keyed by a build hash so it can never
   drift.
2. **Real module identity.** Every module — frozen, disk, builtin,
   extension — gets `__spec__` and `__loader__` from the native
   import path, matching CPython's loader taxonomy
   (`SourceFileLoader` for materialized/disk modules,
   `BuiltinImporter` for native modules, `FrozenImporter` for the
   genuinely frozen `__hello__`/`__phello__` family). `sys.prefix`
   and friends follow the materialized layout, and `site`/`sysconfig`
   describe it truthfully (`_INSTALL_SCHEMES`, `get_paths()`,
   `stdlib`/`platstdlib`/`purelib` keys).
3. **The verbatim tooling stack.** With real files underneath,
   the WeavePy-subset shims for `linecache`, `inspect`, `doctest`,
   `pdb`, `profile`, `pstats`, and `sysconfig` are replaced with
   CPython 3.13's files verbatim per the adoption policy, over a new
   native `_lsprof` (so `cProfile` is real). `test.support`'s
   filesystem expectations (`STDLIB_DIR`, `TEST_HOME_DIR`) become
   true statements.
4. **Long-tail salt.** Shallow, measured engine gaps encountered on
   the way are fixed in the same wave: module `__annotations__`,
   `property`-subclass keyword arguments, `_thread._local`, writable
   `cell.cell_contents`, `AttributeError` (not `TypeError`) shape for
   bound-method attribute stores.

As with every wave since RFC 0036, the deliverable is *measured*: the
full sweep is re-run, `tests/regrtest/expectations.toml` is rewritten
from evidence, and every remaining red carries an actionable
first-failure reason.

## Motivation

1. **`open(module.__file__)` is the ecosystem's favorite trick.**
   pytest rewrites tracebacks by reading source files; coverage.py
   maps executed lines back to files; doctest re-reads the module it
   is testing; `argparse`'s own test suite opens `argparse.__file__`.
   None of these can be special-cased around a filename that does not
   exist. A real file is the only fix with the right fidelity class —
   RFC 0052 proved the pattern of replacing a stash/hack with the
   real mechanism.
2. **The doctest cluster is one root cause wearing six labels.**
   `test_doctest`, `test_cmd`, `test_genexps`, `test_metaclass`,
   `test_pep646_syntax`, `test_zipimport_support` all fail inside
   doctest machinery that needs `linecache` + real source +
   `pdb.set_trace` plumbing. The current `doctest.py` is a 1,216-line
   subset of CPython's 2,919-line original; the missing 1,700 lines
   are mostly the debug/reporting surface those tests probe first.
3. **`test.support` path math silently lies today.**
   `TEST_SUPPORT_DIR = dirname(abspath('<frozen test.support>'))`
   yields garbage; `STDLIB_DIR` and `TEST_HOME_DIR` derive from it.
   Every vendored test that touches `test.support`'s filesystem
   helpers inherits the lie. The frozen override is verbatim CPython
   (RFC 0048) — the *inputs* are what's wrong.
4. **`sysconfig`/`site` shims cap the packaging story.** RFC 0030's
   pip needs truthful install schemes to place scripts/data; `venv`
   needs `sysconfig._get_python_version_abi` and friends; `pydoc`
   wants to link to source files. A CPython-shaped
   `_INSTALL_SCHEMES` over a real prefix retires the whole cluster
   (`test_site`, `test_sysconfig`, `test_venv` first-failures are all
   missing-attribute errors on these shims).
5. **Profiling is a missing accelerator, not missing semantics.**
   `sys.setprofile` fires (RFC 0031); `_lsprof` is just the native
   aggregation layer over it. `test_cprofile`, `test_profile`,
   `test_pstats` fail on `ModuleNotFoundError: _lsprof` /
   `ImportError: SortKey` — pure surface.
6. **Cost of inaction.** The README grades "drop-in" by cloning a
   project and running its test suite. pytest now compiles its
   rewritten ASTs (RFC 0052) — but the first failing test still
   prints a traceback with no source lines, coverage.py cannot map
   its data, and `--pdb` lands in a stub. The tooling substrate is
   the difference between "runs" and "usable".

## CPython reference

- `Modules/getpath.py` + `Python/pathconfig.c` — prefix/exec_prefix
  resolution by landmark search (`os.py` under
  `lib/python{X.Y}`), `PYTHONHOME`, relative-to-executable rules.
- `Lib/importlib/_bootstrap.py` / `_bootstrap_external.py` —
  `ModuleSpec` attributes (`origin`, `has_location`, `cached`,
  `parent`), `SourceFileLoader`, `BuiltinImporter.module_repr`,
  `FrozenImporter` spec shape (`origin='frozen'`).
- `Lib/linecache.py`, `Lib/inspect.py`, `Lib/doctest.py`,
  `Lib/pdb.py`, `Lib/profile.py`, `Lib/pstats.py`, `Lib/sysconfig/`
  — adopted verbatim (3.13) where the wave lands them.
- `Modules/_lsprof.c` + `Lib/cProfile.py` — profiler entry shape
  (`code`/`callcount`/`reccallcount`/`totaltime`/`inlinetime`,
  subentries), `Profiler(timer, timeunit, subcalls, builtins)`.
- `Lib/sysconfig/__init__.py` — `_INSTALL_SCHEMES` (posix_prefix,
  posix_home, venv, …), `get_paths()`, `get_config_vars()`,
  `_get_python_version_abi`.
- `Lib/site.py` — `ENABLE_USER_SITE`, `getsitepackages`,
  `getuserbase`, `venv()` handling of `pyvenv.cfg`.
- `Python/symtable.c` is *not* in scope; the compiler front end
  landed in RFC 0052.
- Acceptance tests: `Lib/test/test_argparse.py`,
  `test_linecache.py`, `test_doctest/`, `test_cmd.py`,
  `test_genexps.py`, `test_metaclass.py`, `test_pep646_syntax.py`,
  `test_unittest/`, `test_cprofile.py`, `test_profile.py`,
  `test_pstats.py`, `test_site.py`, `test_sysconfig.py`,
  `test_venv.py`, `test_py_compile.py`, `test_pydoc.py`,
  `test_module.py`, `test_property.py`, `test_threading_local.py`,
  `test_funcattrs.py`, `test_frozen.py`, `test_sqlite3.py`,
  `test_zoneinfo.py`.

## Detailed design

### WS1 — the materialized `Lib/` tree

**Layout.** The canonical stdlib home is
`{prefix}/lib/weavepy3.13/` (mirroring CPython's
`{prefix}/lib/python3.13/`, with the implementation-specific name
keeping `weavepy` and CPython installs from shadowing each other).
Module name → relative path is derived from the frozen table:
dotted names become directories, `is_package` entries become
`…/__init__.py`, everything else `….py`. Registration aliases that
share one embedded source (`profile`/`cProfile`) each get their own
file; internal storage names (`python/test_support/__init__.py`,
`pdb_mod.py`, …) do **not** leak into the tree — the on-disk names
are the canonical CPython ones (`test/support/__init__.py`,
`pdb.py`).

**Resolution order** (getpath-shaped, in `weavepy-vm`):

1. `WEAVEPYHOME` (and `PYTHONHOME` as an alias) if set: `{home}` is
   the prefix; require the `lib/weavepy3.13/.weavepy-complete`
   landmark (`os` is Rust-native, so `os.py` cannot serve as the
   landmark the way it does in CPython's getpath).
2. Landmark search relative to `sys.executable`: for each ancestor
   `d` of the executable's directory, accept `d` as prefix if
   `d/lib/weavepy3.13/.weavepy-complete` exists (covers installed
   layouts and a future `cargo install` story).
3. Fallback: a per-build cache prefix
   `{user_cache_dir}/weavepy/{build_id}` (macOS
   `~/Library/Caches/…`, Linux `$XDG_CACHE_HOME/…`, Windows
   `%LOCALAPPDATA%\…`), materialized on demand.

**Materialization.** `build_id` is a compile-time FNV-1a hash over
every frozen module's name + source (computed by a `const`-friendly
helper in the stdlib registry) plus the crate version. Extraction is
idempotent and concurrency-safe: write into
`{prefix}.tmp-{pid}`, then `rename` into place; a `COMPLETE` marker
file short-circuits the check to one `stat` on warm starts. The tree
is treated as read-only truth; if a file is missing or the marker is
absent the whole tree is re-extracted. Failure to materialize (read-
only FS, exotic sandbox) degrades gracefully to today's `<frozen X>`
names rather than failing startup.

**Identity wiring.** Once the stdlib dir is known:

- `Interpreter::load_one`'s frozen branch computes the materialized
  path for the module and uses it as both the compile filename
  (`co_filename` on every code object) and `__file__`. The RFC 0021
  frozen-code cache still compiles from the embedded source — the
  path is a *label*, and the build-id key guarantees the label
  matches the bytes on disk.
- `sys.prefix`, `sys.exec_prefix`, `sys.base_prefix`,
  `sys.base_exec_prefix` become the resolved prefix;
  `sys._stdlib_dir` is added; the stdlib dir and its `lib-dynload`
  sibling join `sys.path` after script-dir/`PYTHONPATH` in CPython's
  order.
- The genuinely-frozen demo family (`__hello__`, `__phello__`,
  `__phello__.spam`) is exempt from materialization and instead
  gains a real `FrozenImporter` spec (`origin='frozen'`,
  `__spec__`/`__loader__` set) — `test_frozen`'s probe.

### WS2 — `__spec__`/`__loader__` from the native importer

`build_module_globals` grows loader/spec parameters, and each native
import branch supplies them:

- Materialized-stdlib and on-disk `.py` modules:
  `__loader__ = importlib.machinery.SourceFileLoader(name, path)`,
  `__spec__ = ModuleSpec(name, loader, origin=path)` with
  `has_location=True`, `cached` = the RFC 0033 `__pycache__` path,
  `parent`/`submodule_search_locations` set for packages.
- Builtin native modules: `BuiltinImporter` spec
  (`origin='built-in'`).
- The spec objects are built lazily through a small frozen helper
  (`importlib._bootstrap.module_from_spec` shape is already there)
  so interpreter startup does not import `importlib.machinery`
  eagerly; before `importlib` is loadable the attributes hold
  lightweight placeholders that upgrade on first `importlib` import
  (CPython does the same dance with `_frozen_importlib`).
- `module.__spec__` participation in `repr(module)`, and
  `__cached__` set where a `.pyc` was read/written.

### WS3 — verbatim `linecache`, `inspect`, `doctest`, `pdb`

Adoption-policy replacements (CPython 3.13 files, byte-verbatim,
same test as RFC 0048's `test.support` adoption):

- **`linecache.py`** — verbatim; the `<frozen>` special case and
  loader synthesis are deleted (real files + real `__loader__`
  make them dead code). `_imp.find_frozen` stays for CPython parity
  but stops being load-bearing.
- **`inspect.py`** — verbatim 3,474-line file. Requires: `dis` (RFC
  0033), `ast`, `tokenize` (RFC 0052), `functools`, `enum`,
  `collections.abc` — all present. Engine gaps it exposes (e.g.
  closure-cell writes, method `__dict__`) are WS6 items.
- **`doctest.py`** — verbatim, over verbatim `difflib` (already
  frozen) and the new `pdb`. Restores `debug`, `debug_src`,
  `testsource`, `script_from_examples`, and the
  `REPORT_UDIFF`/`CDIFF`/`NDIFF` output checkers.
- **`pdb.py`** — verbatim over the already-verbatim `bdb.py`;
  `readline` stays an optional import exactly as upstream. The
  `test_pdb` row is re-measured (most of the suite drives pdb
  through scripted stdin, not a live tty; whatever still needs a
  terminal stays enumerated).

### WS4 — truthful `site`/`sysconfig` + packaging coherence

- **`sysconfig`** — replace the 197-line shim with CPython's package
  minus the build-time machinery that cannot apply: a WeavePy
  `_sysconfigdata` module is generated at build time (Rust
  `env!`-driven) carrying the config vars CPython's code expects
  (`prefix`, `LIBDEST`, `BINLIBDEST`, `EXT_SUFFIX`, `SOABI`,
  `Py_GIL_DISABLED: 0`, …). `_INSTALL_SCHEMES` carries
  `posix_prefix`/`posix_home`/`venv`/`nt` with `weavepy3.13`
  substituted where CPython writes `python3.13`;
  `_get_python_version_abi` and the private names `test_venv` and
  pip reach for are included.
- **`site.py`** — verbatim where mechanical; the module already
  mirrors CPython's behavior, so this is mostly adopting the full
  file (`enablerlcompleter` shim for 3.13's removal timeline,
  `getsitepackages` driven by the new prefix).
- `py_compile`, `compileall`, `ensurepip`, `venv` re-run against the
  new layout; measured residuals recorded.

### WS5 — native `_lsprof` + verbatim `cProfile`/`profile`/`pstats`

A new `lsprof_mod.rs` implements `_lsprof.Profiler` over the
RFC 0031 profiling hook infrastructure:

- `enable(subcalls=True, builtins=True)` registers a native profile
  callback (same dispatch point as `sys.setprofile`, but staying in
  Rust — no Python-frame overhead), `disable()`, `clear()`,
  `getstats()` returning CPython-shaped entry structseqs
  (`profiler_entry`/`profiler_subentry` with
  `code`/`callcount`/`reccallcount`/`totaltime`/`inlinetime`).
- Timing via `std::time::Instant` monotonic counts, external-timer
  support (`Profiler(timer, timeunit)`) for the test suite's fake
  clocks.
- `profile.py`, `cProfile.py`, `pstats.py` become verbatim (the
  `SortKey` enum, `Stats(*args, stream=…)`, `FunctionProfile` /
  `StatsProfile` dataclasses).

### WS6 — long-tail engine salt

Fixed alongside, each with a bundled regrtest:

- **Module `__annotations__`**: lazy-created writable getset on
  module objects (CPython `module_get_annotations`), correct
  `AttributeError` text for genuinely missing attributes.
- **`property` subclass kwargs**: route `NativeKind::Property`
  construction through the same kwargs-aware binding as bare
  `property` (fget/fset/fdel/doc by keyword).
- **`_thread._local`**: export the native thread-local type as
  `_thread._local` (aliasing the `_threading_local` implementation
  it already backs).
- **Writable `cell.cell_contents`** (+ `del`), unblocking
  `test_warnings`' `@deprecated` checks and verbatim-`inspect`
  closure paths.
- **Bound-method attribute stores** raise `AttributeError` with
  CPython's message (attributes live on `__func__`), not
  `TypeError`.
- Anything else surfaced by the verbatim adoptions small enough to
  fix in-wave; larger finds are recorded as measured rows.

### WS7 — re-measure and re-baseline

Per the RFC 0049 protocol: two full sweeps
(`weavepy-conformance regrtest --all-cpython --mode subprocess
--jobs 8`), cross-checked; `expectations.toml` rewritten so every
row is measured; stale reasons (several rows still carry wave-5
first-failure text) refreshed where this wave's work moved the
failure point. New bundled regrtests: materialized-tree invariants
(`open(argparse.__file__)` round-trips, `inspect.getsource` on a
stdlib module, build-id staleness re-extraction), spec/loader
identity per module kind, doctest debug surface, `cProfile`-vs-fake-
timer, and the WS6 fixtures.

### Acceptance criteria

1. `open(argparse.__file__).read()` returns the exact embedded
   source; `inspect.getsource(argparse)` and
   `linecache.getlines(argparse.__file__)` agree with it; a
   traceback through a stdlib frame renders real source lines.
2. Every imported module has a non-`None` `__spec__` and
   `__loader__` consistent with its kind; `test_frozen`'s
   `__phello__` probe passes.
3. `test_argparse` and `test_linecache` flip to measured `pass`.
4. At least three of the doctest-driven labels (`test_cmd`,
   `test_genexps`, `test_metaclass`, `test_pep646_syntax`,
   `test_doctest`) flip to measured `pass`.
5. `test_cprofile`, `test_profile`, `test_pstats` flip to measured
   `pass` or to rows whose residuals are enumerated and small.
6. `sysconfig.get_paths()` returns real, existing directories;
   `test_sysconfig`/`test_site`/`test_venv` move past their current
   missing-attribute first failures (measured, residuals
   enumerated).
7. The WS6 fixtures pass; `test_module`, `test_property`,
   `test_threading_local`, `test_funcattrs` flip or their measured
   reasons move past the listed blockers.
8. At least 12 net labels flip red→green on the full sweep versus
   the wave-7 baseline, with no regressions.
9. `cargo fmt` / `clippy -D warnings` / `cargo test --workspace` /
   `regrtest --check` all green.

## Drawbacks

- **Startup now touches the filesystem.** Warm-path cost is one
  `stat` of the `COMPLETE` marker; cold path writes ~460 files once
  per build. Graceful degradation keeps read-only environments
  working (they just keep `<frozen>` names). Accepted: CPython
  *always* reads its stdlib from disk; we only pay a label.
- **Verbatim `inspect`/`doctest`/`pdb` may expose engine gaps
  mid-wave.** That is partly the point — each gap found is a real
  conformance bug. The wave budget reserves WS6 for exactly this;
  anything too large lands as a measured red row, not a shim
  regression (the old subset files are deleted, not kept as
  fallbacks — two implementations is how the current drift
  happened).
- **Two names for one truth** (embedded source vs disk file) invites
  skew. Mitigated structurally: the disk tree is keyed by a hash of
  the embedded bytes and re-extracted wholesale on mismatch; nothing
  ever executes *from* the tree.
- **`weavepy3.13` vs `python3.13` naming** in `sysconfig` schemes may
  surprise tools that hard-code the CPython directory name. This is
  the same trade RFC 0033 made with the `weavepy-3.13` cache tag
  (collision safety beats cosmetic identity), and it matches what
  PyPy ships (`lib/pypy3.10`).

## Alternatives

- **Teach `linecache`/`inspect` to read embedded sources via
  loaders instead of materializing** (PEP 302 `get_source`
  everywhere): rejected — it fixes the introspection stack but not
  `open(module.__file__)`, `os.path.dirname(__file__)` data-file
  math, or `test.support`'s directory expectations; we would keep
  playing whack-a-mole per call-site. The measured `test_argparse`
  residual is literally `open()`.
- **Load the stdlib from disk like CPython (drop the frozen path)**:
  rejected for startup performance — the RFC 0021 frozen-code cache
  is a large win the perf RFCs depend on, and CPython itself froze
  the import-critical stdlib in 3.11 for the same reason. The
  materialized tree gives disk *identity* without disk *execution*.
- **Ship the stdlib inside the binary as a zip on `sys.path`**
  (zipimport-style): rejected — `__file__` inside a zip is still not
  `open()`-able by tests, `.pth`/site-packages layouts don't apply,
  and it forfeits the human-debuggability of a plain tree.
- **Keep improving the subset shims** (`doctest`, `inspect`, `pdb`)
  instead of adopting verbatim: rejected by policy since RFC 0048 —
  every subset shim in this cluster is now the measured first
  failure of some suite, and each hand-port re-diverges on the next
  CPython point release.

## Prior art

- **CPython 3.11+** freezes `importlib`/`os`/`site` for startup but
  still points `__file__` at the on-disk `Lib/` copy — exactly the
  embedded-execution/disk-identity split WS1 adopts (see
  `Python/frozen.c`'s `is_essential_frozen_module` and the
  `_Py_FrozenModule.get_source` arrangement).
- **PyPy** ships `lib-python/3` as a real tree under its own prefix
  name and passes these suites; its `sysconfig` carries
  PyPy-specific scheme names — precedent for `weavepy3.13`.
- **RFC 0048/0050/0051** established the verbatim-adoption policy
  and proved it on `test.support`, `configparser`, `typing`,
  `tokenize`; WS3/WS4 extend it to the introspection stack.
- **RFC 0033** already built the `.pyc`/`__pycache__` machinery this
  wave reuses for `__cached__`/`ModuleSpec.cached`.

## Unresolved questions

- Whether verbatim `inspect` lands wholesale in this wave or the
  hardest corners (e.g. `Signature.from_callable` over every native
  callable kind) leave `test_inspect` as a measured row with an
  enumerated residual. Acceptance criteria allow either.
- Whether `test_pdb` graduates from `skip` to a measured row inside
  the wave budget (verbatim pdb + scripted-stdin support), or stays
  skipped with a refreshed reason.
- Exact `sysconfig` scheme naming for `nt` on Windows (CI covers it;
  the scheme table ships either way).

## Results

A full subprocess sweep against the vendored CPython 3.13
`Lib/test/` (`--mode subprocess --jobs 8 --check`):

```
387 total — pass 233 / fail 137 / error 0 / skip 12 / timeout 5
```

The only divergence observed across two full sweeps is
`test_threading`'s `test_no_refcycle_through_target` — the KNOWN
FLAKE already documented in its expectations row (pre-existing,
reproduces on unmodified main under parallel load; passes in
isolation and 4/5 solo here).

Ten labels flipped this wave, every one traceable to a workstream:

- `test_argparse` (WS1: `open(argparse.__file__)` works; the
  patchable-builtins substrate covers `mock.patch('builtins.open')`),
- `test_linecache` (WS1+WS3: verbatim `linecache` over real files),
- `test_cmd`, `test_genexps` (WS3/WS6: doctest machinery + the
  keyword-genexp SyntaxError),
- `test_profile`, `test_pstats` (WS5: `_lsprof`-backed verbatim
  profiling stack),
- `test_shelve`, `test_timeit`, `test_webbrowser` (verbatim
  adoptions riding the same substrate),
- `test_plistlib` graduated from `timeout` to a measured `fail`
  (three residual expat-shim gaps: utf-16 XML plists and
  entity-declaration rejection).

Suites that the verbatim-`inspect`/`pydoc` adoption initially
regressed were driven back to green inside the wave rather than
re-baselined: `test_enum` (the `object.__dict__['__class__']`
getset), `test_descrtut` (`__slots__` on subclasses of
dict-offset-free builtins), `test_itertools` (weakproxy forwarding
through the new `__class__` descriptor), `test_syntax`
(`__debug__` as a parameter name), `test_zipfile` /
`test_multiprocessing_main_handling` (verbatim `py_compile`'s
`_code_to_timestamp_pyc`/`_write_atomic` in the
`_bootstrap_external` façade), and `test_plistlib`'s
overflow-offset case (`seek()` raises OverflowError, not
TypeError, for over-wide ints).

`cargo fmt`, `cargo clippy --workspace --all-targets`, and
`cargo test --workspace` are green alongside the checked sweep.

## Future work

- **coverage.py end-to-end proof**: with real files + `tokenize` +
  trace fidelity all landed, a bundled fixture installing coverage
  via `_minipip` and asserting a line report is the natural wave-9
  opener.
- **asyncio end-to-end** (native `_asyncio`, subprocess/SSL
  transports, per-submodule grading of the 31-module package) — the
  largest remaining skip, deliberately sequenced after the tooling
  substrate.
- **`zipimport` of the materialized tree** (ship `weavepy313.zip`
  for embedded deployments) once zip `__file__` semantics are worth
  their own RFC.
- Retiring `WEAVEPY_CPYTHON_LIB` in favor of getpath-discovered
  vendor checkouts in the conformance harness.
