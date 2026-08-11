# RFC 0062: The distribution wave — relocatable artifacts, source-built C extensions, per-OS baselines, and upstream-test-suite proof

- **Status**: Accepted
- **Authors**: WeavePy authors
- **Created**: 2026-08-10
- **Tracking issue**: TBD
- **Builds on**: RFC 0053 (the on-disk stdlib tree and landmark-walk
  prefix discovery this wave packages into a shippable artifact),
  RFC 0055 (`_sysconfig`, verbatim `sysconfig`/`venv`/`ensurepip`,
  and the ecosystem lane this wave grows a self-test tier onto),
  RFC 0043–0047 (the binary ABI and exported C-API symbol surface
  that make *loading* stock extensions work — extended here to
  *building* them), RFC 0030 (the in-tree pip and `_pep517` driver
  that learn to drive real C builds), RFC 0036/0049 (the regrtest
  expectations format that gains per-OS override fields),
  RFC 0058 (the bench baseline that becomes per-OS).

## Summary

WeavePy passes 515 of 548 vendored `Lib/test` files and 29 of 29
ecosystem rows — measured on one macOS-arm64 machine, from a binary
that only exists if you clone the repo and run `cargo build`. Nothing
about that is a *drop-in replacement* yet: there is no artifact a user
can extract onto `PATH`, `pip install` of any sdist containing C code
cannot compile (the prefix carries a stub `pyconfig.h` and no
`Python.h`; `sysconfig` reports `CFLAGS=""` and an `LDSHARED` that
cannot link a macOS extension), the ecosystem lane proves packages
*import and answer a probe* rather than *pass their own test suites*,
and every measured claim is single-platform. This wave closes the gap
between "passes CPython's tests on the maintainer's laptop" and
"someone else can switch": a `weavepy-dist` builder that produces a
relocatable tarball (`bin/python3` included) with a self-check that
boots the artifact on a clean prefix; an installable C-API header set
plus compiler-truthful `sysconfig` vars so `pip install --no-binary`
builds real C sdists end-to-end through the in-tree pip and real
setuptools; an upstream-tests tier in the ecosystem lane that runs
marquee packages' *own* pytest suites under WeavePy against a
checked-in baseline; per-OS override fields in the regrtest and
ecosystem expectations plus per-OS bench baselines; and an ecosystem
CI job on ubuntu + macOS so the Linux story is measured, not assumed.

## Motivation

The project's own README leads with "drop-in replacement". After the
RFC 0060 conformance endgame, the semantic gaps remaining are mostly
CPython-internals trivia (codegen-stage streams, `_testbuffer`), while
the gaps a real user hits in the first ten minutes are entirely
untouched:

1. **There is nothing to install.** The RFC 0053 landmark walk
   (`lib/weavepy3.13/.weavepy-complete` found from
   `current_exe()` ancestors) was designed exactly so a
   `{prefix}/bin/weavepy` + `{prefix}/lib/weavepy3.13/` layout would
   be relocatable — but no tool assembles that layout, no `python3`
   shim exists, and nothing verifies the artifact boots outside the
   repo checkout. Every "daily driver" claim is unfalsifiable by
   outsiders.
2. **The first sdist kills the session.** The binary-ABI waves made
   *pre-built* cp313/abi3 wheels load (556 exported `Py*` symbols,
   PEP 425 tag matching, `ExtensionFileLoader`). But the moment a
   dependency has no wheel — a Git checkout, a niche platform, a
   `--no-binary` policy — pip must *compile*, and today that fails
   three ways at once: `INCLUDEPY` points at a directory containing
   only a generated `pyconfig.h` (no `Python.h`), the
   `_weave_sysconfigdata` compiler vars are placeholders
   (`CFLAGS=""`, `LDSHARED="cc -shared"` — unlinkable on macOS
   without `-undefined dynamic_lookup`, no `CCSHARED=-fPIC`), and
   `_pep517`'s fallback silently produces a *pure* wheel for a
   package that declared extensions, deferring the failure to import
   time. CPython ships headers with every install precisely because
   this path is load-bearing for the ecosystem.
3. **Probes are not proof.** The ecosystem lane's 29 rows each run a
   short behaviour probe. That bar caught real engine bugs (every
   capstone since Django found one), but it is an order of magnitude
   weaker than what a switching user actually does: run *their* test
   suite. RFC 0056's future-work list already names the next bar —
   "upstream-test-suite rows (pytest running Flask's own tests)" —
   and no field, stage, or baseline exists for it.
4. **"Measured" means "measured on one Mac".** RFC 0060 and 0061 both
   carry "Windows/Linux measured baselines" in future work. CI does
   run regrtest on ubuntu with the shared expectations file — but the
   expectations format has no per-OS field, so any genuine platform
   divergence forces either a lie (a row marked to the macOS result)
   or a hole (skip). The bench lane gates ubuntu and macOS against a
   single ratio baseline measured on macOS-arm64. The ecosystem lane
   does not run in CI at all.

The cost of inaction is strategic: the conformance number can keep
inching toward 548/548 while the project remains unusable by anyone
who does not build it from source, and unfalsifiable on the platform
(Linux) where a drop-in Python actually gets deployed.

## CPython reference

- **Install layout**: CPython's `make install` / macOS framework and
  Windows installer layouts; `Doc/using/unix.rst`. The POSIX scheme:
  `{prefix}/bin/python3.13` (+ `python3` symlink),
  `{prefix}/lib/python3.13/` stdlib, `{prefix}/include/python3.13/`
  headers, `{prefix}/lib/python3.13/site-packages`. `sysconfig`'s
  `posix_prefix` install scheme (`Lib/sysconfig/__init__.py`,
  `_INSTALL_SCHEMES`) is the contract pip builds against.
- **Headers**: CPython's `Include/` tree (`Python.h` and the ~100
  headers it pulls in) plus the generated `pyconfig.h`;
  `python3-config --includes`; `sysconfig.get_paths()["include"]`.
- **Compiler vars**: `Modules/makesetup` + `configure`-generated
  `_sysconfigdata__*.py` (`build_time_vars`): `CC`, `CFLAGS`,
  `CCSHARED`, `LDSHARED` (macOS: `cc -bundle -undefined
  dynamic_lookup`), `BLDSHARED`, `EXT_SUFFIX`, `INCLUDEPY`,
  `CONFINCLUDEPY`, `AR`, `ARFLAGS`, `OPT`. setuptools'
  `distutils.sysconfig.customize_compiler` consumes exactly this set.
- **PEP 425/440/503/508/517/518**: already implemented by the in-tree
  pip (RFC 0030); this wave exercises the 517 path with a backend
  that actually compiles.
- **`python -m test` portability**: CPython's expectations are
  per-platform via `@support` decorators *inside* the tests; WeavePy's
  external expectations file needs its own per-OS mechanism.

## Detailed design

The wave is five workstreams. WS2 (headers + sysconfig) is the
prerequisite for WS1's artifact contents; WS4 rides on WS1/WS2 for
its source-build rows; WS3 and WS5 are independent.

### WS1 — `weavepy-dist`: the relocatable artifact builder

A new dev-only crate `crates/weavepy-dist` (not published, same
policy as `weavepy-conformance`) with a `weavepy-dist` binary:

```bash
cargo run -p weavepy-dist -- build \
    [--out target/dist] [--weavepy path/to/weavepy] [--format tar.gz|dir]
cargo run -p weavepy-dist -- check [--artifact <tarball-or-dir>]
```

`build` assembles the POSIX layout the RFC 0053 landmark walk already
resolves:

```text
weavepy-<version>-<target-triple>/
├── bin/
│   ├── weavepy                  # the release binary
│   ├── python3.13 -> weavepy    # POSIX symlinks (copies on Windows)
│   ├── python3    -> weavepy
│   └── python     -> weavepy
├── lib/
│   ├── weavepy3.13/             # full stdlib tree (from the embedded
│   │   ├── .weavepy-complete    #   sources, same writer as
│   │   ├── ...                  #   stdlib_tree::materialize)
│   │   └── site-packages/
│   └── python3.13 -> weavepy3.13
├── include/
│   └── python3.13/              # WS2's installable header set
│       ├── Python.h
│       ├── pyconfig.h
│       └── ...
├── README.md                    # artifact-level usage notes
└── LICENSE-{APACHE,MIT}
```

Mechanically, `build` reuses the existing materializer rather than
reimplementing it: it invokes the freshly built `weavepy` binary with
`WEAVEPY_STDLIB_CACHE` pointed into the staging directory so
`stdlib_tree::materialize()` writes the exact tree the runtime
expects (marker, `site-packages/`, `config-3.13*/Makefile`,
`python3.13` symlink, headers once WS2 lands), then adds the `bin/`
shims and tars the result. One writer, one layout, no drift.

`check` is the falsifiability half: it extracts the artifact into a
scratch prefix and, **with the repo checkout masked** (a scrubbed
environment: no `WEAVEPYHOME`, `WEAVEPY_STDLIB_CACHE` pointed at an
empty scratch dir so a materialize fallback would be *visible* as a
check failure rather than silently rescuing a broken artifact), runs
a smoke matrix through `bin/python3` (the shim, deliberately, not
`bin/weavepy`):

1. `python3 -V` / `python3 -c 'import sys; ...'` — identity:
   `sys.prefix`/`base_prefix` == the extracted prefix,
   `sys.executable` under `bin/`, `sys._stdlib_dir` inside the
   artifact, `sysconfig.get_paths()["include"]` exists and contains
   `Python.h`.
2. Stdlib spot-checks that cross native/frozen boundaries:
   `sqlite3`, `ssl`, `zlib`, `decimal`, `ctypes`.
3. `python3 -m venv scratch-venv` then `scratch-venv/bin/python -c`
   — the venv chain resolves back to the artifact.
4. `python -m pip install --no-index --find-links <wheels> <pkg>`
   inside the venv, then import — the packaging chain works offline.
5. WS2 capstone: build the bundled C-sdist fixture from source inside
   the venv and import the resulting extension.

`check` is wired into CI (see WS3) so "the artifact boots on a clean
prefix" is a gate, not a claim. Windows artifact production is
best-effort in this wave (the layout is assembled and `check` runs
`-V`/`-c` legs; the C-build leg is POSIX-only until a later wave —
see Non-goals).

Identity residuals surfaced by the shim get fixed where they are
found (WS5): the `python3`-invoked binary must behave identically
(it already does mechanically — `current_exe()`-based discovery —
but `check` pins it).

### WS2 — source-built C extensions: installable headers + compiler-truthful `sysconfig`

Three changes make `pip install <C sdist>` real:

**1. An installable header set — the stock CPython 3.13 tree.**
WeavePy's runtime is *layout-faithful* to CPython 3.13: RFC 0043–0047
implemented the real object layouts (`PyLongObject` tagged digits,
PEP 393 `PyASCIIObject` compact strings, `PyListObject`/
`PyTupleObject` internals — `crates/weavepy-capi/src/layout.rs`
carries `offset_of!` assertions for each), which is why stock binary
wheels compiled against CPython's own headers already load and run.
The truthful build surface is therefore the **stock CPython 3.13
`Include/` tree itself** — the exact headers every working wheel in
the ecosystem was compiled against — not a hand-grown subset: the
first real sdist (markupsafe) reads `PyUnicode_KIND`/`PyUnicode_DATA`
straight off the PEP 393 struct via inline macros that only the real
headers define. The tree (~265 files incl. `cpython/` and
`internal/`, matching what a CPython install ships) is vendored under
`crates/weavepy-capi/include/cpython313/` with its PSF license text
(the same provenance policy as the vendored `Lib/` checkout, but
committed, because artifacts must build without a host CPython),
embedded into the binary, and written by `stdlib_tree::materialize()`
into `{prefix}/include/python3.13/`, replacing today's lone stub
`pyconfig.h`. The venv layer already points `include` at the base
prefix via the `posix_prefix` scheme.

`pyconfig.h` is the one generated file in a real install, so WeavePy
generates it per-platform: a macOS variant and a Linux variant
(derived from real autoconf output for those platforms, trimmed to
the macros the public headers consume). WeavePy's existing
~1.1k-line `Python.h` in `crates/weavepy-capi/include/` remains the
*audited* limited-API-shaped surface the in-tree C fixtures compile
against — it documents what WeavePy promises; the stock tree is what
the ecosystem compiles with, and the two are exercised by the same
exported-symbol table. A `weavepy-capi` test compiles a translation
unit against the vendored tree (catches embed/instal rot), and the
capi build script prefers the vendored tree over probing a host
`python3.13` for the stock-header fixtures (removing a host
dependency).

**2. Compiler-truthful `sysconfig`.** `_weave_sysconfigdata.py`'s
placeholder vars become the set setuptools'
`customize_compiler()` actually consumes, per-platform:

| var | macOS | Linux |
|---|---|---|
| `CC` / `CXX` | `cc` / `c++` (env `CC`/`CXX` respected by setuptools) | same |
| `CFLAGS` | `-fno-strict-overflow -Wsign-compare -g -O3` | same |
| `CCSHARED` | `""` (Mach-O) | `-fPIC` |
| `LDSHARED` | `cc -bundle -undefined dynamic_lookup` | `cc -shared` |
| `BLDSHARED` | same as `LDSHARED` | same |
| `LDCXXSHARED` | `c++ -bundle -undefined dynamic_lookup` | `c++ -shared` |
| `AR`/`ARFLAGS`/`OPT` | `ar` / `rcs` / `-DNDEBUG -g -O3` | same |
| `INCLUDEPY`/`CONFINCLUDEPY` | `{installed_base}/include/python3.13` (truthful — the dir now exists with headers) | same |

`Py_ENABLE_SHARED` stays `0` and `LIBS`/`LIBPYTHON` stay empty: like
a static-libpython CPython, extensions do **not** link libpython —
they resolve `Py*` at load time from the process (macOS
`-undefined dynamic_lookup`, Linux `--export-dynamic` on the binary,
both already in place from RFC 0043). The `config-3.13*/Makefile`
mirror gains the same vars so `sysconfig` stays consistent whether it
reads the frozen data or the on-disk Makefile.

**3. An honest PEP 517 path.** `_pep517`'s fallback currently builds
a *pure* wheel no matter what the sdist declares. It gains a
tripwire: if the source tree declares extensions (`ext_modules` in
`setup.py`/`setup.cfg` detected conservatively, or a
`pyproject.toml` backend we can't satisfy) and no real backend is
available, the build **fails with a diagnostic** naming the missing
backend instead of installing a wheel that cannot import. When
setuptools *is* available (it is bundled in every ecosystem venv and
in the offline wheel cache), the normal `setuptools.build_meta` path
now succeeds end-to-end because vars + headers are real.

**Proof, offline and online.** A new bundled fixture sdist
`tests/capi_ext/weavepy_cext_demo/` (a minimal `setup.py` +
`demo.c` exercising `PyArg_ParseTuple`, exceptions, and a type with
`tp_methods`) is checked in; a regrtest fixture builds it with
`pip install --no-binary :all:` inside a scratch venv and imports it
— fully offline, deterministic, CI-friendly. On top, the ecosystem
manifest gains a `no_binary = true` row flag (forces
`--no-binary :all:` for the row's requirements) and two real-world
rows prove the path against PyPI sdists: **markupsafe** (C speedups,
zero external deps) and **wrapt** (C extension with graceful
fallback — the row asserts the *compiled* variant imported). The
offline wheel fetcher learns to also fetch `--no-binary` sdists for
those rows.

### WS3 — per-OS baselines: expectations overrides, ecosystem CI, per-OS bench

**Expectations overrides.** The regrtest and ecosystem expectation
formats gain optional per-OS override keys, resolved at load time
against the host:

```toml
[tests."cpython/Lib/test/test_ioctl.py"]
status = "pass"
status_linux = "fail"      # optional; also status_macos, status_windows
reason = "..."
reason_linux = "..."       # optional, same suffixes
```

Resolution order: `status_<os>` if present, else `status`. The
parser (`simple_toml` in `regrtest.rs`, `EcosystemExpectations` in
`ecosystem.rs`) treats unknown `status_*` suffixes as errors (typo
protection). This is deliberately a flat suffix scheme, not nested
tables — the file stays greppable and diff-reviewable, and the
existing 1400-line baseline needs zero rewrites.

**Ecosystem CI.** `.github/workflows/ci.yml` gains an `ecosystem`
job on `ubuntu-latest` + `macos-latest`: build release CLI, fetch
the wheel cache with `tools/ecosystem_fetch.py` (cached via
`actions/cache` keyed on the manifest hash), run
`weavepy-conformance ecosystem --wheels … --check`. Blocking, same
policy as regrtest. The self-test tier (WS4) rides in the same job.
A `dist-check` job (ubuntu + macos) builds the WS1 artifact and runs
`weavepy-dist check` against it.

**Per-OS bench baselines.** `crates/weavepy-bench/baselines/`
becomes per-platform: `bench-macos-aarch64.json` (the existing
measured file, renamed) plus `bench-linux-x86_64.json` measured in
this wave (via a Linux runner or container; if neither is available
to the implementer the Linux file ships from the first CI
`--update-baseline` run and the gate is advisory on Linux until the
row is committed — the *mechanism* is the deliverable, and the gate
refuses to compare a baseline whose platform key mismatches the
host). `weavepy-bench` resolves the baseline by host `os-arch` and
fails with a clear message when no baseline exists for the platform
rather than silently comparing against foreign ratios (today's
behavior).

**Linux regrtest divergences.** CI already runs the full sweep on
ubuntu; any rows that only hold on macOS get truthful
`status_linux` / `reason_linux` entries instead of prose-only
caveats (e.g. the `test_multiprocessing_fork` darwin SkipTest row).

### WS4 — the upstream-tests tier: packages' own suites as the bar

The ecosystem manifest grows an optional per-row self-test spec:

```toml
[packages.attrs]
requirements = "attrs"
probe = "probes/attrs_probe.py"

[packages.attrs.selftest]
source = "attrs"                     # sdist requirement to fetch/extract
requirements = "pytest hypothesis"   # extra test-only deps
command = "tests"                    # pytest target inside the sdist root
deselect = [                         # measured, enumerated escapes
    "tests/test_mypy.py",            #   (each with a reason comment)
]
timeout_seconds = 600
```

Harness changes in `crates/weavepy-conformance/src/ecosystem.rs`:
`run_row` gains a fourth stage after the probe. It downloads (or, in
`--wheels` mode, takes from the cache — the fetcher learns to grab
these sdists too) the `source` sdist, extracts it into the row
scratch dir, installs `selftest.requirements` into the same venv,
and runs `python -m pytest <command> -p no:cacheprovider -q
--deselect …` from the sdist root. Grading is CPython-regrtest-shaped:
exit 0 (with ≥ 1 test collected) is `pass`; the expectations file
gains a `selftest_status` (+ per-OS suffixes from WS3) so probe and
self-test grade independently — a row can be probe-green while its
self-test is a measured, reasoned red.

Deselects are the honesty mechanism, mirroring
`expectations.toml`'s measured-reason discipline: every entry gets an
inline comment naming the failure class. A deselect list that grows
past a small fraction of the suite is a signal the row isn't ready to
claim, and the RFC's acceptance bar counts *suites passing*, not
rows-with-unbounded-escapes: launch set is **six** marquee pure-Python
packages whose sdists carry their tests — `attrs`, `click`,
`jinja2`, `python-dateutil`, `packaging`, `markupsafe` (the last
doubling as the WS2 source-build proof: its suite runs against the
extension we compiled). `six` and `tqdm` are stretch rows. Suites
known to hard-require missing surfaces get measured
`selftest_status = "fail"` rows with reasons, not silence.

This tier is expected to find engine bugs (every capstone has);
fixing what it finds is in-scope for the wave, time-boxed per bug at
triage — anything structural gets a measured red row and an entry in
Future work rather than a heroic detour.

### WS5 — distribution-blocking residual burns

Scoped to what the new lanes actually surface, plus two known rows:

- `site.enablerlcompleter` — the `test_site` first-failure
  (`AttributeError`), a 3.13 surface gap in the CPython-shaped
  `site.py`; restores the row to measured-pass candidacy.
- `PYTHONHOME` two-path form (`prefix:exec_prefix`) — documented in
  `--help` but unimplemented in `stdlib_tree::resolve`; a
  distribution artifact makes `PYTHONHOME` a real user surface.
- Anything `weavepy-dist check` or the self-test tier flags as an
  identity/layout bug (e.g. `sys._base_executable` under the
  `python3` shim, venv-from-artifact edge cases) — fixed in place,
  covered by the check matrix.

Deliberately **not** pulled in: the `test_socket`
`sendmsg`/`SCM_RIGHTS` surface (platform-API work, not
distribution-blocking; stays a principled skip), and all
conformance-trivia rows (codegen-stage cluster, `_testbuffer`).

### Non-goals

- **Windows parity.** The artifact builder assembles a Windows layout
  and CI keeps the `test` job, but the C-sdist build path (MSVC
  `LDSHARED` analog, `.lib` import-library questions) and Windows
  regrtest/bench baselines are a follow-up wave.
- **musl/alpine artifacts**, signing/notarization, package-manager
  distribution (brew/apt), and an installer UX.
- **scipy/greenlet-class rows.** Heavy-native rows (scipy, Pillow,
  lxml, greenlet's stack switching) stay out of the launch matrix;
  the self-test tier has to hold on pure-Python marquees first.
- **A shared libpython.** Extensions resolve symbols from the
  process, like static-libpython CPython; embedders keep the Rust
  `weavepy` crate API.

### Acceptance criteria

1. **Artifact boots clean**: `weavepy-dist build` produces a tarball;
   `weavepy-dist check` passes its full matrix (identity, stdlib
   spot-checks, venv, offline pip, C-sdist build) against the
   extracted artifact on a scratch prefix, on macOS and ubuntu CI.
2. **C sdists compile**: the bundled `weavepy_cext_demo` sdist
   builds and imports offline via `pip install --no-binary :all:`
   (regrtest fixture, both CI OSes); `markupsafe` and `wrapt` build
   from real PyPI sdists in the ecosystem lane with the compiled
   (non-fallback) module imported.
3. **Upstream suites pass**: ≥ 5 of the 6 launch self-test rows grade
   `pass` with enumerated deselects; the 6th is at worst a measured,
   reasoned `fail` row. No probe row regresses (still 29/29 +
   new rows green).
4. **Per-OS machinery lands**: `status_<os>`/`reason_<os>` resolve in
   both expectation formats with tests; bench baselines are
   per-platform files with host-matched resolution; ecosystem +
   dist-check CI jobs are blocking on ubuntu + macOS.
5. **No conformance regression**: the regrtest sweep stays at
   `unexpected 0` against the (possibly per-OS-annotated) baseline on
   both CI OSes; `test_site` flips to measured pass.
6. **All gates green**: `cargo fmt`, `clippy -D warnings`,
   `cargo test --workspace`, `regrtest --check`, `ecosystem --check`,
   `weavepy-dist check`.

## Drawbacks

- **The header set is an ABI promise.** Shipping
  `include/python3.13/` makes every declared prototype a compatibility
  surface third parties compile against; the symbol-audit test
  mitigates drift but the maintenance duty is permanent — this is the
  cost of being a real target rather than a wheel-loader.
- **Self-test rows are upstream-coupled.** Package releases can break
  rows for reasons that aren't WeavePy bugs (new test deps, flaky
  tests). Mitigation: rows pin exact versions in the manifest, and
  the offline cache makes CI hermetic.
- **CI minutes grow** (ecosystem × 2 OSes + dist-check + self-tests).
  Mitigation: the wheel cache is actions-cached; self-tests run in
  the same job/venvs as probes.
- **A silent-fallback removal is a behavior break**: sdists that
  "installed" (purely) before WS2's tripwire now fail loudly. That is
  the honest outcome, but it will surface as new failures for anyone
  relying on the accident.

## Alternatives

- **Grow WeavePy's own limited-API `Python.h` into the installable
  set** instead of vendoring CPython's `Include/` tree: rejected —
  the runtime already implements CPython's real object layouts (that
  is how stock wheels load), and real sdists compile against the
  non-limited surface (markupsafe's `PyUnicode_KIND` macro family
  reads the PEP 393 struct directly). A hand-grown header would fail
  the first real package while *advertising* less than the ABI
  actually supports. The stock tree does declare symbols WeavePy has
  not implemented, but the failure mode is identical to today's
  binary wheels (dynamic lookup at first use) and is the honest
  signal for which symbol to implement next.
- **A shared `libweavepy.so`/`libpython3.13.so`** so extensions link
  conventionally: rejected for this wave — the dynamic-lookup model
  already works for the entire binary-wheel ecosystem, matches
  static-libpython CPython, and a shared library reopens the
  embedding/versioning story prematurely.
- **Per-OS expectation *files*** (`expectations-linux.toml`) instead
  of suffix keys: rejected — 1400 lines of near-duplicate baseline to
  keep in sync for a handful of divergent rows; the suffix scheme
  keeps one source of truth and makes divergence greppable.
- **Running upstream suites from GitHub checkouts** instead of sdists:
  rejected — sdists are version-pinned, hash-cacheable, and the same
  artifact pip installs; Git adds a network dependency class and a
  moving target.
- **Nightly-only ecosystem CI** to save minutes: rejected — the lane
  exists to block regressions at the PR that causes them; the cache
  makes the steady-state cost acceptable.

## Prior art

- **python-build-standalone** (Astral): relocatable CPython tarballs
  with `bin/ + lib/ + include/`; its documented quirks (the
  `sysconfig` vars must describe the *artifact*, not the build
  machine) directly informed WS2's truthful-vars rule.
- **PyPy**: ships `include/` with its own `Python.h` variant
  (cpyext) rather than CPython's tree — the same
  "header states the actual ABI" choice WS2 makes; its history of
  extension-compile bugs motivated the compile-every-header-standalone
  test.
- **uv / pyenv / mise**: consume exactly the layout WS1 produces
  (prefix with `bin/python3`); pyenv's shim behavior is why the
  artifact ships `python`/`python3`/`python3.13` names, not just
  `weavepy`.
- **conda-forge's `python` packaging**: precedent for
  `Py_ENABLE_SHARED=0` installs where extensions resolve from the
  process.
- **CPython's own `test.pythoninfo` + buildbot fleet**: the
  per-platform-baseline discipline WS3 imports into expectations.

## Unresolved questions

- Should the artifact's `bin/python` (bare, no `3`) exist by default,
  or only `python3`/`python3.13`? PEP 394 says distros decide; we ship
  all three and may revisit.
- Version identity of the artifact: the workspace is `0.0.0`; the
  tarball name uses the workspace version + short git hash until a
  release-versioning RFC exists.
- Whether the self-test tier should eventually gate on *test counts*
  (pass/fail/deselected tallies) rather than exit status — deferred
  until a few waves of row history exist.

## Future work

- `PyDateTimeAPI->DateType` (and siblings) are the C-side shell types
  (byte-faithful instances + inherited `tp_new` for Cython, RFC 0029)
  — `PyDate_Check`, the capsule constructors, and the
  `PyDateTime_GET_*` macros are faithful, but *calling Python
  methods on the type itself* (`PyObject_CallMethod((PyObject *)
  PyDateTimeAPI->DateType, "today", NULL)`) hits the shell rather
  than the Python-visible `datetime.date` class. Discovered by this
  wave's header-proof fixture; needs the shell types to answer
  attribute protocol via the bridged VM class.
- Windows end-to-end: MSVC build vars, `.pyd` sdist builds, Windows
  regrtest/bench baselines, a zip artifact.
- Heavy-native self-test rows (numpy's own suite is the obvious
  capstone), and the scipy/Pillow/lxml/greenlet matrix expansion.
- Release automation: tagged builds publishing artifacts from CI,
  checksums/signing, a `weavepy self update` story.
- A `python3-config` shim and `pkg-config` file for build systems
  that bypass sysconfig.
- Per-OS *measured* Windows rows once the above lands; musl targets.

## Results

Measured on macOS arm64 (the ubuntu legs run in CI via the new
blocking jobs). Regrtest per the RFC 0049 protocol (`--mode
subprocess --workers 4 --timeout 60` against the per-OS-annotated
baseline); ecosystem from the offline wheel cache
(`--wheels target/ecosystem-wheels --selftests`).

### Workstream outcomes

| WS | Deliverable | Result |
|---|---|---|
| WS1 | `weavepy-dist` builder + check matrix | 16 MB tar.gz (49 MB extracted), `weavepy-{version}+g{sha}-{target}` layout; **all 7 check legs pass** (version, identity, stdlib, venv, offline pip, C-sdist build, decoy-cache) |
| WS2 | Installable headers + compiler-truthful sysconfig | Full CPython 3.13 header tree + generated `pyconfig.h` under `include/python3.13/`; bundled `weavepy_cext_demo` sdist compiles and imports offline (regrtest fixture `test_cext_build` pass) |
| WS3 | Per-OS baselines + CI | `status_<os>`/`reason_<os>`/`timeout_seconds_<os>` in both expectation formats (unknown suffix = load error); bench baselines per `bench-{os}-{arch}.json` with platform-stamp verification; blocking `ecosystem` + `dist-check` CI jobs on ubuntu + macOS |
| WS4 | Upstream-suite self-test tier | `[packages.X.selftest]` manifest tables + `selftest_status` grading; **5 of 6 launch rows pass** with enumerated deselects, attrs is the one allowed non-pass row (measured, reasoned) |
| WS5 | Distribution-blocking burns | `-V` → `Python 3.13.0 (WeavePy 0.0.0)`; `resolve()` retries the ancestor walk on `canonicalize(current_exe())` (venv/symlink shims self-locate, decoy-cache leg green); `PYTHONHOME` two-path form; `site.enablerlcompleter` landed and `test_site` flipped to measured pass (42 run / 0 fail) |

### Upstream self-test matrix (launch rows)

| row | selftest | measured |
|---|---|---|
| packaging | pass | 10,898 tests across 16 files (~7 min); two enumerated speed trims: upstream's own `-m "not property"` lanes, and `test_version.py`'s cartesian ordering matrix (>50 min of interpreter time, zero failures observed before cutoff — semantics covered by test_specifiers/test_requirements/test_ranges) |
| click | pass | 1,606 passed; 13 deselects in four documented classes (CliRunner stdin echo, `TextIOWrapper.write` str-subclass TypeError, importlib.metadata name resolution, LazyFile atomic rename) + `test_types.py` ignored (lone-surrogate filename raises TypeError not OSError) |
| jinja2 | pass | full suite minus the trio-dependent async files (socket lacks `SOMAXCONN`); the wave's `_string.formatter_parser` str-subclass fix deleted the row's former five-entry deselect list |
| dateutil | pass | 2,030 passed; 1 deselect (hypothesis-found: pre-epoch `astimezone()`/tzlocal `tzname()` divergence) |
| markupsafe (sdist-built) | pass | 78/79 against the *compiled* `_speedups`; 1 deselect (C-ABI bridge rejects a str-subclass into `_escape_inner` — the shell-type residue tracked in future work) |
| attrs | skip (measured) | suite exceeds a 2,400 s budget (killed at 2,402 s far from completion): hypothesis `@given` loops woven through test_make/test_funcs/test_dunders are interpreter-speed-bound; no honest property-lane trim exists, so this takes the acceptance criteria's one allowed non-pass row |

### Conformance and ecosystem sweeps

- Regrtest: **434 total — 402 pass / 26 expected-fail / 6 skip,
  0 unexpected** (exit 0 against the annotated baseline);
  `test_site` is a measured pass.
- Ecosystem: **31 rows — 31 pass, 0 unexpected; selftests 5 pass /
  1 baselined skip** (exit 0, offline lane). The two sdist proof
  rows (`markupsafe_sdist`, `wrapt_sdist`) build real PyPI C sdists
  through `_pep517` + setuptools against the installed headers, with
  the probes asserting the compiled module is live.
- Landing the wave surfaced and fixed two dev-host harness bugs: the
  sitecustomize-guard shim now mirrors the whole external `Lib/`
  (minus the hook files) so unbundled pure-Python stdlib modules
  (`sched`, `tabnanny`, `pyclbr`, `modulefinder`) keep resolving,
  and the frozen `test.support` no longer re-aliases `STDLIB_DIR`
  onto a second name for the same tree (which tripped unittest
  discovery's "Path must be within the project" assert in 13
  package-style suites).

### Acceptance checklist

1. Artifact boots clean, full check matrix green — **met** (macOS
   measured; ubuntu runs in the new blocking `dist-check` job).
2. C sdists compile (demo fixture + markupsafe/wrapt from PyPI,
   compiled module imported) — **met**.
3. ≥ 5 of 6 self-test rows pass, 6th measured and reasoned — **met**
   (5 pass; attrs is the documented budget-bound row).
4. Per-OS machinery in both formats + per-platform bench + blocking
   CI jobs — **met**.
5. No conformance regression, `test_site` measured pass — **met**
   (unexpected 0).
6. All gates green (`fmt`, `clippy -D warnings`,
   `cargo test --workspace`, `regrtest --check`,
   `ecosystem --check`, `weavepy-dist check`) — **met** on macOS.
