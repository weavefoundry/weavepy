# RFC 0075: The embedding wave — C lifecycle parity, a shipped libpython, and ecosystem wave 5's upstream-selftest burn

- **Status**: Accepted
- **Authors**: WeavePy authors
- **Created**: 2026-08-26
- **Tracking issue**: TBD
- **Builds on**: RFC 0055 (whose WS4 minted the `test_embed`/`test_getpath`
  principled skips this wave retires), RFC 0064 (the `weavepy-pylib`
  cdylib and `Py_Main`/`Py_BytesMain` twins this wave grows into a real
  embedding surface), RFC 0062 (the relocatable artifact the shared
  libpython and `python3-config` join), RFC 0031/0068 (PEP 684
  sub-interpreters, which `Py_NewInterpreter` now fronts from C), RFC
  0072 (ecosystem wave 4, whose grpcio red row and numpy re-census this
  wave burns), RFC 0066 (the `installed`-mode selftest lane scipy/
  Pillow/lxml now join), RFC 0049 (measured-baseline protocol).

## Summary

WeavePy's conformance scoreboard reads zero — fail 0, error 0, timeout
0, unexpected 0 — but two of the three surviving skips are not
CPython-faithful skips; they are confessions. `test_embed` and
`test_getpath` are skipped because **the C embedding surface does not
exist**: `Py_Initialize` works only because the host is already the
interpreter, `Py_Finalize` is an empty function, there is no
`PyConfig`, no `PyRun_*` family, and POSIX ships no linkable
libpython. An application that embeds CPython — mod_wsgi, a
PyO3-`auto-initialize` Rust binary, a pybind11 `scoped_interpreter`
host, Blender-shaped plugin runtimes — cannot even *try* WeavePy.

This wave lands both halves of the remaining drop-in story in one
commit:

1. **Embedding & lifecycle parity (WS1–WS6).** The PEP 587
   `PyPreConfig`/`PyConfig`/`PyStatus` surface with
   `Py_InitializeFromConfig` and `Py_RunMain`; the `PyRun_*` execution
   family (`PyRun_SimpleString`, `PyRun_String`, `PyRun_File`,
   `PyRun_AnyFile`, `PyRun_InteractiveLoop`, `Py_CompileString`,
   `PyEval_EvalCode`) plus `PyImport_AppendInittab` for embedder
   built-ins; a **real `Py_FinalizeEx`** (atexit, non-daemon thread
   join, stdio flush, module teardown, GC, and a re-initialisable
   interpreter so init→fini→init cycles work); `Py_NewInterpreter`/
   `Py_EndInterpreter` over the RFC 0031/0068 sub-interpreter
   machinery; a shipped `libpython3.13.{so,dylib}` +
   `python3-config` + pkg-config file in the `weavepy-dist` layout
   with a compile-link-run embedding smoke in the dist self-check;
   and a `_testembed` twin binary so `test_embed` and `test_getpath`
   graduate from principled skips to **measured rows**.
2. **Ecosystem wave 5 — the upstream-selftest burn (WS7–WS9).** The
   lane's only red probe row (**grpcio**: the cygrpc server-poller
   thread dies on the first RPC) is root-caused and burned; the
   **numpy `selftest_status = "fail"`** census (281 failures, four
   enumerated clusters, a collection budget overrun) is driven to a
   pass-with-enumerated-deselects verdict; **scipy, Pillow, and
   lxml** gain upstream-selftest lanes; **scikit-learn** joins as the
   next matrix rung (joblib/loky process pools over scipy); and the
   deployment-shaped capstone runs **gunicorn `-k gevent`** serving a
   Django app end-to-end — the worker class RFC 0072 called the
   default production topology.

As with every wave since RFC 0036, the deliverable is measured: the
regrtest sweep re-runs at `unexpected 0` with `test_embed`/
`test_getpath` as measured rows, the ecosystem manifest grows from 40
to ≥ 43 rows with grpcio green and fresh selftest verdicts, and every
touched expectations row is rewritten from evidence.

## Motivation

1. **Embedding is the last surface that literally does not exist.**
   Every other skip in `tests/regrtest/expectations.toml` is
   CPython-faithful (`test_multiprocessing_fork` on Darwin matches
   CPython's own bpo-33725 skip). The `test_embed`/`test_getpath`
   reasons, by contrast, say "not meaningful for a Rust interpreter"
   — which was true when the C-API was a loader shim and is false now
   that WeavePy ships `python313.dll` on Windows and a 725-symbol
   ABI. A drop-in claim with no `#include <Python.h>; Py_Initialize()`
   story is qualified in the way embedder-adjacent adopters notice
   immediately.
2. **`Py_Finalize` as a no-op is a correctness bug, not a stub.** An
   embedder that runs init→exec→fini→init today gets a second
   interpreter contaminated by the first's `sys.modules`, atexit
   queue, and thread registry. CPython's `test_embed` spends most of
   its runtime on exactly this (`test_repeated_init_exec`,
   `test_repeated_init_and_subinterpreters`); we cannot claim the
   lifecycle without it.
3. **The ecosystem lane's honesty demands the burn.** grpcio is the
   only red probe row and its reason says "needs deeper thread-state
   work than this wave scopes" — a named debt. numpy's
   `selftest_status = "fail"` row carries a fresh, fully-clustered
   census (scalar `__text_signature__` 96, ufunc object-loops 32,
   cast-safety 28, mmap-as-buffer 24) that is precisely a worklist.
   Leaving a measured worklist unworked is the lane's definition of
   drift.
4. **The selftest lanes are the best remaining fuzzers.** Every
   heavy-native wave (RFC 0057, 0060, 0069, 0072) caught real
   segfault-class engine bugs. scipy/Pillow/lxml suites and
   scikit-learn's loky process pools reach C-API and process-model
   surfaces no probe touches. The gunicorn capstone rebinds sockets,
   signals, fork, and greenlet switching in one process — the
   deployment shape itself.
5. **Cost of inaction.** Perf waves (the 0074-chartered frame-coverage
   stratum) do not change *who can run* WeavePy; this wave converts
   the two remaining "cannot run" answers — embedders and
   upstream-selftest skeptics — into measured surfaces.

## CPython reference

- **PEP 587** (Python Initialization Configuration) and
  `Doc/c-api/init_config.rst`: `PyStatus` (a by-value struct with
  `exitcode`/`err_msg`/`func`; `PyStatus_Ok/Error/NoMemory/Exit`,
  `PyStatus_Is{Error,Exit}`, `Py_ExitStatusException`),
  `PyPreConfig_InitPythonConfig/InitIsolatedConfig` +
  `Py_PreInitialize[FromArgs|FromBytesArgs]`,
  `PyConfig_InitPythonConfig/InitIsolatedConfig`,
  `PyConfig_SetString/SetBytesString/SetArgv/SetBytesArgv/
  SetWideStringList`, `PyConfig_Read`, `PyConfig_Clear`,
  `Py_InitializeFromConfig`, and `Py_RunMain` (consumes the config's
  `argv`/`run_command`/`run_module`/`run_filename`, returns the exit
  code, finalises). The config struct layout in
  `Include/cpython/initconfig.h` is the ABI spec for the twin.
- **`Python/pylifecycle.c`**: `Py_FinalizeEx`'s documented sequence —
  wait for non-daemon threads (`threading._shutdown`), call atexit
  callbacks, flush `sys.stdout`/`sys.stderr`, garbage-collect,
  destroy sub-interpreters, tear down modules (`sys.modules` cleared
  with the `builtins`/`sys` last discipline), release runtime state
  so `Py_Initialize` can run again. `Py_AtExit`'s 32-slot
  C-callback table. `Py_NewInterpreter`/`Py_EndInterpreter` semantics
  (own `sys.modules`, shared process; `Py_EndInterpreter` refuses the
  main interpreter).
- **`Python/pythonrun.c`**: the `PyRun_*` family and flag plumbing —
  `PyRun_SimpleString[Flags]` (module `__main__`, prints tracebacks,
  returns -1/0), `PyRun_String[Flags]` (returns the object,
  start-token `Py_eval_input`/`Py_file_input`/`Py_single_input`),
  `PyRun_[Any]File[Ex][Flags]` (`closeit` contract),
  `PyRun_InteractiveOne/Loop` (`sys.ps1`/`ps2`, E0F returns),
  `Py_CompileString[ExFlags]` (returns a code object with
  `optimize`/`PyCompilerFlags`), `PyEval_EvalCode`.
- **`Programs/_testembed.c`**: the command-dispatch binary
  `Lib/test/test_embed.py` drives (`main(argc, argv)` → named test
  functions: `test_repeated_init_exec`, `test_forced_io_encoding`,
  `test_init_*` config-dump commands that print the live config as
  JSON via `_testinternalcapi.get_configs`, audit-hook probes,
  `test_repeated_init_and_subinterpreters`). WeavePy builds a twin
  against its own headers implementing the subset the vendored test
  file exercises; CPython-build-specific legs (frozen-module blobs,
  `Py_TRACE_REFS`) become enumerated `divergence_tests` rows exactly
  like `test_marshal`'s.
- **`Modules/getpath.py`**: CPython's path-computation script,
  interpreted by `test_getpath.py` under a fully mocked host. The
  file is a build artifact outside `Lib/`, hence absent from the
  vendored tree; vendoring it (verbatim, 3.13.x) makes the test a
  pure-Python exercise of *CPython's* algorithm under WeavePy's
  interpreter — no runtime coupling.
- **`Misc/python-config.in` / `Misc/python.pc.in`**: the
  `python3-config --cflags/--ldflags/--embed` contract and the
  pkg-config `python-3.13-embed.pc` module that build systems
  (CMake's `FindPython`, meson's `pymod.dependency(embed: true)`)
  consume.
- **Package specs under test (WS7–WS9)**: grpcio (Cython bindings
  over the C++ core; the server drives completion-queue polling from
  Python daemon threads that re-enter via `PyGILState_Ensure`), numpy
  2.5.2 (`numpy._core` suite; the four clusters named in the RFC 0072
  census), scipy/Pillow/lxml sdist/installed suites, scikit-learn
  (joblib/loky: `multiprocessing` spawn + memmapped arrays), gunicorn
  (`-k gevent`: pre-fork master, `os.fork`, SIGTERM/SIGQUIT
  discipline, gevent monkey-patching in workers).

## Detailed design

### WS1 — PyStatus, PyPreConfig, PyConfig, Py_InitializeFromConfig

New module `crates/weavepy-capi/src/initconfig.rs`.

**`PyStatus`** is returned *by value*: a `#[repr(C)]` struct
(`_type: c_int`, `func: *const c_char`, `err_msg: *const c_char`,
`exitcode: c_int`) with constructors/predicates as `#[no_mangle]`
functions. Error strings are `'static` or leaked-once C strings —
CPython's own are static literals, so no ownership protocol is
needed.

**`PyPreConfig`** (`#[repr(C)]`, field-for-field with
`Include/cpython/initconfig.h`): `parse_argv`, `isolated`,
`use_environment`, `configure_locale`, `coerce_c_locale{,_warn}`,
`utf8_mode`, `dev_mode`, `allocator`. `Py_PreInitialize` records the
pre-config in a process-global; WeavePy is UTF-8-native so
`utf8_mode`/locale coercion are accepted and reflected in
`sys.flags`, not re-implemented.

**`PyConfig`** mirrors the 3.13 struct layout exactly (the twin is
ABI-visible: embedders allocate it on their stack and poke fields
directly). Wide-string fields (`home`, `program_name`,
`executable`, `run_command`, `run_module`, `run_filename`,
`pythonpath_env`, `stdio_encoding`, …) and `PyWideStringList`
(`argv`, `xoptions`, `warnoptions`, `module_search_paths`) get the
full setter surface (`PyConfig_SetString`, `PyConfig_SetBytesString`,
`PyConfig_SetArgv`, `PyConfig_SetBytesArgv`,
`PyConfig_SetWideStringList`, `PyWideStringList_Append/Insert`) with
`decode_wide_arg`-style WTF-safe decoding (the `weavepy-pylib`
helper moves down into `weavepy-capi` so both crates share it).
`PyConfig_Read` fills defaults (argv[0]-derived `program_name`,
computed `module_search_paths` via the existing RFC 0053 landmark
walk, environment unless `use_environment == 0`) and flips
`_config_init` to the read state. `PyConfig_Clear` frees everything
it allocated (embedders may have set fields with their own
`PyMem_RawMalloc` strings; ownership follows CPython: `Clear` frees
all pointer fields unconditionally, so setters always copy).

**`Py_InitializeFromConfig(config)`** translates the config into the
existing `weavepy_vm::Interpreter` bootstrap: `isolated` ⇒ skip env +
user site, `run_*`/`argv` stored for `Py_RunMain`, `home`/
`program_name`/`module_search_paths` override the landmark walk,
`verbose`/`quiet`/`inspect`/`optimization_level`/`dont_write_bytecode`
land in `sys.flags`. It fails with `PyStatus_Error` (not abort) on
unreadable config. **`Py_RunMain`** then executes the stored
command/module/filename through the same driver `weavepy-cli` uses
(`cli_main_with_args` grows a from-config entry that skips argv
parsing), finalises, and returns the exit code.

The interpreter-owning side changes: today `ensure_initialised()`
assumes the host binary made an interpreter. WS1 gives `weavepy-capi`
an **owned-interpreter mode**: when `Py_Initialize*` is called and no
`ACTIVE`/`LAST_INTERPRETER` exists, the capi crate constructs a real
`Interpreter` (boxed, leaked into a process-global slot), installs
the extension loader, and publishes it exactly as the CLI does. This
is the piece that turns "the host is already the interpreter" into
"the caller might be a C program".

### WS2 — the PyRun_* execution family and embedder built-ins

New module `crates/weavepy-capi/src/pythonrun.rs`:

- `PyRun_SimpleString[Flags]`: compile+exec in `__main__`'s dict,
  print the traceback via the existing error-display path on failure,
  return 0/-1. `SystemExit` propagates as process exit per CPython.
- `PyRun_String[Flags](str, start, globals, locals)`: start tokens
  `Py_eval_input`(258)/`Py_file_input`(257)/`Py_single_input`(256)
  exported as constants in the twin headers; returns a new reference
  or NULL-with-exception.
- `PyRun_File`/`PyRun_FileEx`/`PyRun_AnyFile[Ex][Flags]`: read the
  whole `FILE*` through `libc::fread` (the `closeit` contract calls
  `libc::fclose`); `AnyFile` dispatches to the interactive loop when
  the fd is a tty.
- `PyRun_InteractiveOne/Loop[Flags]`: line loop over the `FILE*`
  with `sys.ps1`/`sys.ps2`, reusing the CLI REPL's incomplete-input
  detection (`codeop`-shaped) so multi-line blocks work from C.
- `Py_CompileString[ExFlags]` → real code object via the existing
  compile-from-source path (`optimize` plumbed to the RFC 0057
  compile-flags surface); `PyEval_EvalCode(code, globals, locals)`
  evaluates it.
- `PyImport_AppendInittab/ExtendInittab`: a pre-init table of
  `(name, PyInit_*)` pairs consulted by the import machinery before
  the extension loader — the standard embedder pattern for injecting
  a built-in module. Calling it after init fails per CPython.

All entry points self-serve the GIL (`PyGILState_Ensure` discipline)
so a bare C `main` that calls `Py_Initialize(); PyRun_SimpleString(…)`
works without touching thread APIs, matching CPython's
main-thread-implicit-GIL behavior.

### WS3 — real finalization and re-initialisable lifecycle

`Py_FinalizeEx` (and `Py_Finalize` over it) leaves stub-land and runs
CPython's documented sequence against the owned or host interpreter:

1. `threading._shutdown()` — join non-daemon threads; mark daemon
   threads unusable (their next GIL acquisition parks forever, the
   CPython 3.13 contract).
2. Run `atexit` callbacks (Python level), then the `Py_AtExit` C
   table (32 slots, LIFO, registered post-init).
3. Flush and detach `sys.stdout`/`sys.stderr`.
4. Final garbage collection with finalizers; `gc.collect()` twice
   per CPython's unreachable-resurrection discipline.
5. Tear down sub-interpreters created by `Py_NewInterpreter` that
   the embedder leaked (CPython destroys them in fini).
6. Clear `sys.modules` (builtins/sys last), drop the module registry,
   and — the WeavePy-specific part — **reset the process-global
   singletons the VM caches** (interned strings survive; type
   registry, import state, signal handlers, `LAST_INTERPRETER`, the
   capi owned-interpreter slot are cleared) so a subsequent
   `Py_Initialize` builds a genuinely fresh interpreter.
7. Return 0, or -1 if flushing raised (the `Py_FinalizeEx` contract).

`Py_IsInitialized` becomes truthful (a process-global state enum:
`Uninitialised → Initialised → Finalising → Uninitialised`), and
`ensure_initialised()` asserts against use-after-fini instead of
silently resurrecting. The regrtest evidence is `test_embed`'s
`test_repeated_init_exec` (init→exec→fini ×N with clean state each
round).

The one honest limit: WeavePy's `sync::Rc = Arc` heap means objects
an embedder holds across fini stay valid (no CPython-style
use-after-free), but the *interpreter services* behind them are gone.
This is strictly safer than CPython and documented as such.

### WS4 — Py_NewInterpreter / Py_EndInterpreter from C

Thin C fronts over the RFC 0031/0068 PEP 684 machinery
(`_xxsubinterpreters`): `Py_NewInterpreter()` creates a
sub-interpreter with legacy (shared-GIL) config, swaps the calling
thread's active context to it, and returns its `PyThreadState*`;
`Py_NewInterpreterFromConfig(&tstate, &config)` takes the 3.12+
`PyInterpreterConfig` (own-GIL requests are accepted and coerced to
shared-GIL with the documented `PyStatus` warning — WeavePy has one
GIL; the sub-interpreter isolation semantics are what PEP 684
consumers actually depend on). `Py_EndInterpreter(tstate)` destroys
it and refuses the main interpreter with a fatal error, per contract.
`PyThreadState_Get/Swap` stop being pure sentinels: the
`crate::pystate` per-thread store grows an interpreter-id field so
`Swap` actually routes subsequent capi calls to the right
interpreter's modules — the piece `test_embed`'s
`test_repeated_init_and_subinterpreters` exercises.

### WS5 — the shipped shared libpython, python3-config, pkg-config

`weavepy-dist build` grows the embedding kit on POSIX:

- `lib/libpython3.13.{so,dylib}` — the `weavepy-pylib` cdylib,
  installed with the CPython-conventional soname (symlink chain
  `libpython3.13.so → libpython3.13.so.1.0` on Linux; install-name
  `@rpath/libpython3.13.dylib` on macOS). The static-binary CLI
  ships unchanged; the shared object is *additive* for embedders
  (mirroring CPython's own `--enable-shared` layout where `python3`
  and `libpython` coexist).
- `bin/python3-config` — a generated POSIX-sh script implementing
  `--cflags/--includes/--ldflags/--libs/--embed/--prefix/
  --exec-prefix/--extension-suffix/--abiflags/--configdir`, printing
  paths relative to the relocatable prefix (computed from `$0`, the
  RFC 0053 landmark discipline — CPython's is configure-substituted;
  ours must relocate).
- `lib/pkgconfig/python-3.13.pc` + `python-3.13-embed.pc` +
  `python3-embed.pc` symlink, `${pcfiledir}`-relative so the tarball
  relocates.
- The dist **self-check grows an embedding smoke**: compile
  `smoke_embed.c` (`Py_InitializeFromConfig` + `PyRun_SimpleString`
  + `PyImport_AppendInittab` + init→fini→init cycle) with `cc
  $(python3-config --cflags --embed --ldflags --embed)` against the
  extracted prefix, run it under an `LD_LIBRARY_PATH`/`@rpath`-clean
  environment, and assert output. Windows keeps its existing
  `python313.dll` posture (RFC 0064); the smoke there links the
  import library already shipped.

### WS6 — the _testembed twin; test_embed and test_getpath as measured rows

**`tests/capi_ext/_testembed.c`** (built by the same fixture
machinery as `_greenletconsumer.c`, but as an *executable*, which the
fixture builder learns to produce): implements the command surface
the vendored `test_embed.py` dispatches on — the `test_repeated_init_
exec` / `test_forced_io_encoding` / `test_init_*` config-dump family
— against WeavePy's WS1 config surface. The config-dump commands
print the live config JSON the way `_testinternalcapi.get_configs`
does; that internal helper grows the missing fields. The regrtest
harness learns an `embed-binary` hook: `test_embed.py` finds the twin
via the same `sysconfig`-derived path CPython uses (we publish it
under the prefix the vendored test computes).

**Grading**: `test_embed` flips from `skip` to a measured row. Target
is `pass` with CPython-build-specific subtests (frozen-blob layout,
`Py_TRACE_REFS` legs, allocator-domain accounting) enumerated in
`divergence_tests` with per-test reasons — the `test_marshal`
mechanism. Whatever the sweep measures is what lands; a `fail` row
with a first-failure reason is the documented fallback, but the skip
retires either way.

**`test_getpath`**: vendor CPython 3.13's `Modules/getpath.py`
verbatim under `vendor/cpython/Modules/getpath.py` and teach the
vendored test's source-tree probe to find it (it locates the file
relative to the checkout root; the vendored tree gains the one
directory). The test then runs CPython's path algorithm under mocked
hosts — pure Python, no WeavePy runtime coupling — and grades on its
own merits. Target: measured `pass`.

### WS7 — grpcio: the completion-queue burn

Root-cause posture from the RFC 0072 census: the `_serve` poller
thread dies with an *empty traceback* the moment an RPC arrives —
the signature of exception machinery breaking in a
`PyGILState_Ensure`-attached foreign thread, not of grpc logic
failing. Suspect surfaces, in probe order:

1. **Thread-state attach/detach churn**: cygrpc's poller detaches and
   re-attaches per completion-queue tick (`with nogil` blocks around
   `grpc_completion_queue_next`). WeavePy's `PyEval_SaveThread`
   returns a dangling sentinel and `PyGILState_Ensure` a 0/1 token —
   correct for balanced pairs, but cygrpc *interleaves* GILState and
   SaveThread on the same thread. The WS4 pystate work (real
   per-thread state objects with an interpreter id) replaces both
   sentinels with actual state, which this row then validates.
2. **Exception propagation across `except *` Cython signatures**: an
   empty traceback suggests `PyErr_Fetch`-family calls returning
   inconsistent triples from a foreign thread; the pystate
   per-thread exception slots must be keyed by the *attached* state.
3. **Fork/exec handlers**: grpcio registers `pthread_atfork`
   handlers; inert here (probe is single-process) but audited.

The probe stays as RFC 0072 defined it (in-process server, generic
bytes handlers, unary echo with deadline). Acceptance: the row flips
to `pass`; if a residual survives, its reason must name a *new*
root cause — "needs deeper thread-state work" is spent by WS4.

### WS8 — the numpy selftest burn

Four clusters, one root cause each (RFC 0072 census), burned in
descending size:

1. **Scalar `__text_signature__` (96 failures)**: `inspect.signature`
   over C scalar constructors — the capi type-creation path drops
   `__text_signature__` extraction from docstrings for static types'
   `tp_new`/`tp_init` docs. Land the CPython `_PyType_DocWithoutSignature`
   / `__text_signature__` split in the type getset surface (the
   `wave5.rs` doc plumbing already parses the `sig)\n--\n\n` marker
   for methods; extend it to types and `PyGetSetDef` members).
2. **ufunc object-loops + ufunc pickling (32)**: object-dtype inner
   loops call back into the VM per element — audit the borrowed-ref
   discipline in `PyUFunc_*` object loops (suspect: the RFC 0069
   container-owned reference fix needs the ufunc-loop analogue);
   ufunc pickling needs `copyreg._reconstructor`-shaped support for
   the `numpy._core._multiarray_umath` module-attribute reduce path.
3. **numeric→datetime cast safety (28, DID-NOT-RAISE)**: the capi
   cast-probe (`PyArray_CanCastTypeTo` consumers) — WeavePy's
   `PyNumber_Index`/`__index__` bridge is looser than CPython's on
   foreign scalars; tighten to CPython's exact TypeError surface.
4. **mmap objects as buffer sources (24)**: `mmap.mmap` doesn't
   export a C-visible buffer through the capi `PyObject_GetBuffer`
   bridge (VM-level buffer protocol exists; the identity-box bridge
   lacks the mmap arm). Wire it like bytearray's.

**The budget**: collection alone measured 2170s of the 2400s budget.
The lane splits per-module — the manifest `command` already accepts
arbitrary pytest args; the selftest runner learns a `shards` key
(array of `--pyargs`-relative module groups run as separate pytest
invocations under one row, verdict = worst shard). Honest fallback if
sharding is disproportionate: a measured budget raise with the fresh
collection number in the row comment.

Acceptance: `selftest_status = "pass"` with every remaining failure
an enumerated `deselect` carrying a measured reason, or — documented
fallback — a `fail` row whose census is current and whose clusters
name *new* root causes.

### WS9 — scipy/Pillow/lxml selftest lanes, scikit-learn, the gunicorn capstone

- **scipy**: `installed` mode, scoped to `--pyargs scipy.linalg
  scipy.sparse scipy.fft` (the C-API-heavy heart; full-suite runtime
  is a known non-starter under current perf — the scope is the row
  comment). **Pillow**: sdist mode (`Tests/` ships in the sdist),
  helpers vendored per upstream layout. **lxml**: sdist mode over the
  pinned wheel's matching sdist (`src/lxml/tests`), doctest-heavy.
  Each row grades `selftest_status` per the RFC 0066 protocol:
  measured verdict, enumerated deselects, budget in the manifest.
- **scikit-learn**: new probe row — `sklearn.linear_model
  .LogisticRegression` fit/predict on synthetic data,
  `RandomForestClassifier` with `n_jobs=2` (the loky process-pool
  leg), `pipeline` + `GridSearchCV` (2×2), and a `joblib.Memory`
  cache round-trip. This is the first row exercising loky's
  spawn-with-memmap process model.
- **gunicorn `-k gevent` capstone**: a new probe launches `gunicorn
  --workers 2 -k gevent` serving the RFC 0056 Django app under the
  scratch venv's WeavePy, drives concurrent requests through the
  patched-socket path (readiness-polled, SIGTERM'd, exit-code
  asserted), and asserts worker responses + clean master shutdown.
  Rides the WS7-adjacent signal/fork surfaces
  (`pre-fork master → os.fork → monkey.patch_all` in child).
- Rows land in `manifest.toml` + `expectations.toml`;
  `tools/ecosystem_fetch.py` learns the new pins so the offline
  `--wheels` lane covers them; CI cache keys pick up the manifest
  hash automatically.

### WS10 — re-measure and re-baseline

Per the RFC 0049 protocol: full regrtest sweep (`--mode subprocess
--jobs 8`) at `unexpected 0` with the two graduated rows; ecosystem
lane re-run online and offline; every touched row rewritten from
evidence; new bundled regrtests for every engine fix (standing
policy); `cargo fmt` / `clippy -D warnings` / `cargo test
--workspace` green.

### Acceptance criteria

1. A C program compiled with `cc $(python3-config --cflags --embed
   --ldflags --embed)` against the extracted dist prefix runs
   `Py_InitializeFromConfig` → `PyRun_SimpleString` →
   `Py_FinalizeEx` → re-init cleanly on macOS and Linux (the dist
   self-check embedding smoke). Windows continues to link
   `python313.dll` (existing posture) and gains the same smoke over
   the import library.
2. `PyConfig`/`PyPreConfig`/`PyStatus` match the 3.13 struct layouts
   (a layout assertion test against `Include/cpython/initconfig.h`
   offsets joins the `layout.rs` suite); `Py_RunMain` drives
   `run_command`/`run_module`/`run_filename` with CPython exit-code
   semantics.
3. `Py_FinalizeEx` runs the documented teardown sequence;
   init→fini→init ×3 leaves no cross-cycle state (asserted by the
   `_testembed` twin's `test_repeated_init_exec`).
4. `test_embed.py` and `test_getpath.py` are **measured rows** in
   `tests/regrtest/expectations.toml`; the "principled skip" reasons
   are retired. Target `pass` (with `divergence_tests` enumeration
   where CPython-build-specific); measured `fail`-with-reason is the
   documented fallback — a skip is not.
5. The **grpcio row passes** (unary echo through the real cygrpc
   server) on macOS and Linux, offline from the wheel cache.
6. numpy `selftest_status` reaches a fresh measured verdict per WS8
   with the four named clusters burned or re-root-caused; the
   collection-budget blocker is resolved (shards or measured raise).
7. scipy/Pillow/lxml selftest lanes and the scikit-learn row land
   measured; the gunicorn `-k gevent` capstone passes on macOS and
   Linux.
8. Full sweep `unexpected 0`; ecosystem `--check` green online and
   offline; `cargo fmt` / `clippy -D warnings` / `cargo test
   --workspace` green.

## Drawbacks

- **Finalization touches every global.** The VM's process-global
  singletons (type registry, interned strings, import state) were
  designed init-once; making them resettable risks regressions in
  the 550-label sweep for a feature most CLI users never invoke.
  Mitigation: the reset path is exercised only via `Py_Finalize`;
  the CLI keeps its exit-without-fini fast path; the sweep re-runs
  in-wave.
- **A shipped shared library doubles the POSIX artifact surface.**
  Soname discipline, rpath correctness, and symbol-visibility drift
  become release concerns POSIX never had here. Mitigation: the
  dist smoke compiles and runs against the extracted tarball in CI;
  the cdylib already built everywhere since RFC 0064, so only the
  *shipping* is new.
- **The `_testembed` twin is a maintenance tail.** test_embed grows
  legs with every CPython point release; the twin must track the
  vendored test file, not upstream HEAD. Mitigation: the twin lives
  next to the other capi fixtures and is graded by the same sweep
  that would catch drift.
- **grpcio and the numpy clusters are open-ended debugging.** The
  RFC scopes root causes from a measured census, but C++
  completion-queue internals and ufunc reference discipline can
  fractal. Mitigation: measured-row discipline — a red row with a
  *fresh* reason is an acceptable landing for the selftest lanes
  (not for grpcio's probe row, which is this wave's acceptance).
- **Wheel-cache growth**: scikit-learn + gunicorn + sdists for
  Pillow/lxml/scipy suites add ~150 MB per platform. Same posture as
  RFC 0056/0066/0072: pins in the manifest, CI cache keyed on it.

## Alternatives

- **PEP 741 (`PyInitConfig`) instead of PEP 587**: rejected for this
  wave — 741 is the 3.14 surface; the 3.13 drop-in target and every
  embedder shipping today (mod_wsgi, PyO3 0.2x, pybind11) speak 587.
  741 joins the 3.14 gap analysis.
- **Ship the shared library as the only artifact (CPython
  `--enable-shared` style)**: rejected — the fully-static CLI is a
  measured startup win and the RFC 0062 relocation story depends on
  it. The dist ships both; embedders link the .so, users run the
  static binary.
- **Keep test_embed skipped and test embedding with our own fixtures
  only**: rejected — the whole wave exists because "not meaningful
  for a Rust interpreter" stopped being true; a bespoke fixture
  cannot retire a skip whose reason names CPython's test.
- **Re-implement getpath semantics natively instead of vendoring
  `getpath.py`**: rejected — WeavePy's RFC 0053 landmark walk *is*
  its path semantics, already tested via test_sysconfig/venv/site;
  test_getpath tests CPython's script, and vendoring the script is
  the honest (and cheap) way to run it.
- **Split this into two commits (embedding wave, then ecosystem
  wave 5)**: viable, and the history's usual cadence — rejected here
  per the explicit request for one landing, and because WS4's
  pystate rework is grpcio's (WS7) prerequisite: landing them
  together means the thread-state story is validated by a real C++
  consumer in the same sweep, not one wave later.
- **Scope numpy's verdict to "budget fixed, clusters carried"**:
  rejected as a target (the clusters are measured worklists, and two
  of the four are plausibly single-fix), kept as the documented
  fallback shape.

## Prior art

- **CPython's own embedding kit** (`python3-config`, `python-3.13-embed.pc`,
  `--enable-shared` coexisting with a static binary) is the layout
  WS5 mirrors; the relocatable twist follows python-build-standalone,
  which patches `python-config` for prefix-relative output — the same
  `$0`-derived discipline WS5 generates directly.
- **PyPy** ships `libpypy3.9-c.so` and a cpyext `Py_Initialize`;
  its documented lesson — embedders interleave `PyGILState_*` and
  `PyEval_SaveThread` in ways no test suite admits — is WS4's
  motivation for real per-thread states over sentinels.
- **GraalPy** grades CPython's `test_embed` against its own
  `graalpy-config` twin with enumerated per-test exclusions — the
  `divergence_tests` shape WS6 adopts.
- **The RFC 0064 `python313.dll` wave** already proved the "whole
  interpreter as a linkable library" build on Windows; WS5 is its
  POSIX symmetrization, deferred there as a non-goal and called due
  here.
- **RFC 0066/0072 selftest-lane discipline** (installed mode, budget
  rows, deselect-with-reason) is reused verbatim for WS8/WS9.

## Unresolved questions

- **How much of `test_embed`'s config-dump matrix is satisfiable.**
  The `test_init_*` legs compare full config JSON; fields WeavePy
  accepts-but-coerces (allocator domains, `configure_locale`) may
  need `divergence_tests` rows. The twin is built first and the
  matrix measured before grading posture is chosen.
- **Whether daemon-thread parking at fini is implementable without
  the full CPython tstate-pointer dance.** The GIL layer can park
  attachers on a fini flag; the open question is C threads *inside*
  `Py_BEGIN_ALLOW_THREADS` at fini time — CPython hangs them on
  re-acquire; WeavePy must match without corrupting the owned-
  interpreter teardown. Measured by the twin's daemon-thread leg.
- **grpcio's failure may be deeper than thread-state.** If the empty
  traceback survives real per-thread states, the next suspects are
  cygrpc's `except *` exception-check protocol over our
  `PyErr_Occurred` and the C++ core's fork handlers. The row's
  acceptance stands; the reason-naming discipline covers the risk.
- **scipy's selftest scope.** `--pyargs scipy.linalg scipy.sparse
  scipy.fft` is a judgment call; if the measured runtime allows,
  `scipy.special`/`scipy.ndimage` join. The row comment records the
  chosen scope and why.
- **Whether `Py_NewInterpreterFromConfig`'s own-GIL coercion is
  acceptable to real consumers.** PEP 684 consumers today
  (`test_interpreters`, `_interpchannels`) don't require true
  per-interpreter GILs; an embedder that does gets the documented
  `PyStatus` warning. The free-threading design note (RFC 0072 debt,
  still owed) inherits this boundary.

## Future work

- **PEP 741 `PyInitConfig`** and the rest of the 3.14 gap analysis
  (RFC 0068 debt).
- **mod_wsgi / PyO3-embed as ecosystem rows**: with WS5 shipped, an
  `embedding` lane rung — a PyO3 `auto-initialize` Rust consumer and
  a pybind11 `scoped_interpreter` C++ consumer built against the
  dist prefix — is the natural wave-6 proof.
- **The free-threading / PEP 703 design note** (RFC 0066/0072 debt):
  WS3's lifecycle state machine and WS4's real per-thread states are
  prerequisites it inherits.
- **Full-suite scipy/scikit-learn selftests** once a perf wave makes
  the budgets sane; the scoped rows are the beachhead.
- **Perf waves 11+** (the RFC 0074 charter: closure cells, escaping
  callees, allocation elision) — unchanged, and now also the path to
  un-skipping the attrs selftest.
