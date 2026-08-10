# RFC 0060: Conformance endgame — the `_testcapi` fixture surface, introspection constructors, and the long-tail burn

- **Status**: Accepted
- **Authors**: WeavePy authors
- **Created**: 2026-08-08
- **Tracking issue**: TBD
- **Builds on**: RFC 0049/0057 (the measured whole-suite baseline and the
  burn-down protocol this wave continues), RFC 0033 (code-object surface
  this wave makes constructible), RFC 0028/0044 (the C-fixture build lane
  `tests/capi_ext/` this wave grows), RFC 0055/0056 (the ecosystem lane
  this wave extends with two new rows), RFC 0031 (observability hooks
  this wave finishes auditing).

## Summary

After RFC 0057 and the two performance waves, the measured `Lib/test`
baseline stands at **496 of 543 files passing** with 41 measured `fail`
rows and 6 principled skips; the ecosystem lane is 27/27. This wave is
the endgame burn on those 41 rows. A fresh per-row re-measurement (all
41 rows re-run on the post-RFC-0059 binary, 2026-08-08) shows the tail
is *not* 41 independent problems — it clusters, and the largest cluster
is not engine behavior at all:

1. **The `_testcapi` / `_testinternalcapi` fixture surface (~10 rows).**
   `test_capi` errors on nearly every test for want of `_testcapi`
   submodule attributes; `test_call` dies at *import* on
   `_testcapi.MethInstance` (the vectorcall fixture types);
   `test_fileutils` wants `_testinternalcapi.normalize_path`,
   `test_optimizer` wants `reset_rare_event_counters`,
   `test_dict_version` wants dict-watcher probes, `test_frame` wants the
   frame C-API probes, `test_compile`'s `TestInstructionSequence` and
   both `test_compiler_{assemble,codegen}` want
   `_testinternalcapi.new_instruction_sequence` + `assemble_code_object`
   + `optimize_cfg` (shared with the `test_peepholer` E-cluster), and
   `test_import`'s SubinterpImportTests want `_testsinglephase` /
   `_testmultiphase`. CPython treats these as *test fixtures*, and so do
   we: the wave lands them as a mix of native modules and C-extension
   fixtures in the existing `tests/capi_ext/` lane, implemented against
   real engine behavior (the dict version tags, rare-event counters, and
   frame APIs already exist internally — the fixtures expose them).
2. **Introspection constructors (~5 rows).** `types.CodeType(...)` is
   not constructible from Python (`test_code`'s first error), which also
   gates `test_dis`'s import-time helpers; `type()` rejects tuple
   *subclasses* as bases (`test_types`); `types.FunctionType(code,
   globals, ...)` defaults handling (`test_types`); marshal rejects
   buffer inputs (`array`/`memoryview` — `test_marshal`) and the
   `version` argument tail.
3. **The `hashlib` accelerator surface (1 dense row).** blake2b/blake2s
   with the full parameter block (digest_size/key/salt/person/tree
   parameters), sha3 family + shake XOFs — the E/F flood in
   `test_hashlib` is constructors and parameter validation, not
   digest-core bugs (sha3/shake already back `hashlib.sha3_*`).
4. **Module metadata truthfulness (~4 rows).** Materialized-stdlib
   modules lack `__file__` (`os.__file__` — `test_import`'s first
   error), `sys.orig_argv` is absent (`test_sys`), the bundled
   `test.libregrtest` shim shadows the vendored real one and lacks
   `TestStats` (`test_regrtest`), frozen from-import error shapes
   (`test_import`).
5. **Observability residuals (~5 rows).** `sys.monitoring` exception
   events for async constructs (`test_monitoring`), the
   `test_sys_settrace` residual (49F: `frame_setlineno` block-analysis
   parity), `trace`-module CLI legs (`test_trace`), the
   audit-hook-blocking semantics (`test_audit`: a denied `addaudithook`
   must not register the hook), `sys.call_tracing` arity.
6. **The `re` residual (1 row).** Measured F/E cluster in `test_re`:
   `re.sub` group-expansion edges, template-parse errors, and the
   Unicode-property/atomic-group tail RFC 0051 enumerated.
7. **Stdlib odds and ends (~15 rows).** Each with a fresh measured
   first-failure: `test_builtin` (chr/OverflowError taxonomy + an E/F
   spread), `test_inspect` (getfullargspec over builtin methods),
   `test_email` (one early E + policy tail), `test_pathlib` (an
   E-cluster in glob/walk), `test_pydoc` (doc-rendering shapes),
   `test_site` (user-site residuals; the `test_license_exists_at_url`
   leg is network-gated), `test_source_encoding` (long coding-name
   cookies), `test_zoneinfo` (the weak-cache trio),
   `test_urllib2_localnet` (an HTTPS leg), `test_logging` (a
   SyncManager multiprocessing leg pushing 49s), `test_fork1`
   (threaded import-lock fork), `test_file_eintr` (one readlines
   case), `test_resource` (RLIM_INFINITY-adjacent negative values),
   `test_context` (contextvars getset), `test_ctypes` (from_buffer_copy
   + the frozentable residual), `test_ast` (a single
   non-string-keyword error-message case), `test_frame` (the non-CAPI
   half: f_lineno del/segfault guards), `test_importlib` (extension
   loader edges).

The wave also lands the RFC 0056-style ecosystem capstone: **pandas**
and **FastAPI (with uvicorn serving a live request)** join the lane as
rows 28 and 29.

As with every wave since RFC 0036, the deliverable is measured: a full
re-baseline sweep, every touched row rewritten from evidence, reds
allowed with reasons mandatory, `unexpected 0`.

## Motivation

1. **The remaining tail is now dominated by fixtures, not semantics.**
   The fresh re-measurement shows the single biggest blocker class is
   missing *test support* surface (`_testcapi` and friends). Landing it
   converts an opaque "42 red rows" into a mostly-green sweep plus a
   small, honestly-enumerated semantic residual — and un-gates entire
   suites (`test_call` and `test_dis` currently die at import, so their
   *hundreds* of call-protocol and disassembly tests contribute zero
   signal today).
2. **"Drop-in" is conjunctive across the suites auditors run first.**
   `test_capi`, `test_code`, `test_call`, `test_re`, `test_hashlib` are
   exactly the rows a skeptical "is it really a drop-in?" audit reaches
   for. Every one is in this wave's scope.
3. **The fixture lane already exists.** RFC 0028/0044/0029 built
   `tests/capi_ext/` (C fixtures compiled against our `Python.h` and
   loaded through the real `ExtensionFileLoader`); RFC 0048 maintains
   dict version tags; RFC 0059's rare-event counters exist for the JIT
   guards. The wave is mostly *exposing* real machinery, which is why
   it is tractable in one commit.
4. **Ecosystem credibility compounds.** pandas is the most-requested
   "does it run?" package WeavePy has passed only as a source-built
   test-suite milestone (pre-RFC-0039); a graded, offline-reproducible
   lane row is the honest version of that claim. FastAPI+uvicorn is the
   modern service stack (pydantic v2 + anyio already pass).
5. **Cost of inaction.** Every future wave keeps re-measuring the same
   41 rows; the fixture gap keeps suppressing signal from ~1,500
   individual tests inside import-blocked suites.

## CPython reference

- `Modules/_testcapimodule.c` + `Modules/_testcapi/*.c` — the fixture
  surface: `MethInstance`/`MethClass`/`MethStatic` (vectorcall METH
  probes), heap-type factories, `pyobject_*` probes, buffer fixtures.
- `Modules/_testinternalcapi.c` — `normalize_path`,
  `reset_rare_event_counters` + `get_rare_event_counters`,
  `new_instruction_sequence`, `assemble_code_object`, `optimize_cfg`,
  dict-watcher / type-watcher / func-watcher probes.
- `Modules/_testsinglephase.c`, `Modules/_testmultiphase.c` — the
  import-machinery fixtures for single/multi-phase init.
- `Objects/codeobject.c` (`code_new` — the 18-argument constructor and
  its validation order), `Objects/typeobject.c` (`type_new` accepting
  tuple subclasses for bases), `Objects/funcobject.c`
  (`func_new` — defaults/closure validation).
- `Modules/_blake2/` (parameter block: `digest_size`, `key`, `salt`,
  `person`, `fanout`, `depth`, `leaf_size`, `node_offset`,
  `node_depth`, `inner_size`, `last_node`), `Modules/_sha3/` (Keccak,
  `shake_{128,256}` XOF `digest(length)`).
- `Python/marshal.c` — `PyObject_GetBuffer` acceptance in
  `marshal.dumps`/`loads` inputs, the `version` parameter, and
  `TYPE_*` completeness.
- `Lib/test/audit-tests.py` (`test_block_add_hook`: a hook that raises
  from the `sys.addaudithook` event blocks registration),
  `sys.call_tracing`, `sys.orig_argv` (PEP 587 shape).
- `Modules/_sre/sre_lib.h` — possessive/atomic backtracking cut
  semantics; `Lib/re/_parser.py` template parsing (the `re.sub`
  expansion edges `test_re` measures).
- `Modules/_zoneinfo.c` weak-cache semantics; `Lib/logging/handlers`
  + `Lib/multiprocessing/managers` (SyncManager leg);
  `Modules/faulthandler.c` for the `test_frame` segfault-guard cases.
- Acceptance suites: every row named in the Summary clusters, graded
  by `tests/regrtest/expectations.toml` under the RFC 0049 protocol.

## Detailed design

### WS1 — the `_testcapi` fixture surface

Grow the native `_testcapi` / `_testinternalcapi` modules (they exist —
RFC 0040/0057 landed slices) to the attribute sets the ten gated rows
actually touch, in dependency order:

- **Vectorcall fixtures** (`test_call`): `MethInstance`, `MethClass`,
  `MethStatic`, `pyfunc_with_vectorcall`, the `VectorCallClass` family,
  `pyobject_vectorcall`/`pyobject_fastcalldict`, and
  `PyVectorcall_Call` probes. These are native types whose slots call
  straight into the RFC 0028 vectorcall machinery — the point is to
  exercise *our* call paths, not to stub.
- **Instruction-sequence + CFG fixtures** (`test_compile`,
  `test_compiler_assemble`, `test_compiler_codegen`,
  `test_peepholer`): `_testinternalcapi.new_instruction_sequence`
  (builds on the RFC 0033 `cpython_code` codec's instruction model),
  `assemble_code_object`, `optimize_cfg`, `compiler_codegen`. Where
  CPython's flowgraph produces a shape our compiler does not, the
  fixture routes through a faithful port of the flowgraph
  transformations we already implemented for jump threading
  (RFC 0057 WS6) — the suites are the spec.
- **Watcher probes** (`test_dict_version`, parts of `test_capi`):
  dict/type/func watchers over the RFC 0048 version-tag machinery.
- **Rare-event counters** (`test_optimizer`): expose the RFC 0059
  guard-invalidation counters (`set_class`, `set_bases`,
  `set_eval_frame_func`, `builtin_dict`, `func_modification`).
- **`normalize_path`** (`test_fileutils`): port `_Py_normalize_path`.
- **Frame C-API probes** (`test_frame`): `frame_getlocals`,
  `frame_new`, `frame_fback` etc. over our real frame objects.
- **Import fixtures** (`test_import`): `_testsinglephase` /
  `_testmultiphase` as real C fixtures in `tests/capi_ext/`, compiled
  and installed the way `_ndarray.c`/`_numpylike.c` already are.
- **The `test_capi` package** is graded per-submodule reality: each
  `test_capi/test_*.py` leg that exercises surface we genuinely have
  gets its fixtures; legs probing CPython-only internals with no
  public contract (refcount exactness, allocator internals) keep a
  measured red with the leg enumerated in the row's reason — the
  RFC 0049 honesty rule, not forced green.

### WS2 — introspection constructors

- **`types.CodeType(...)`**: the full 18-argument Python-level
  constructor with CPython's validation order and error messages,
  mapping onto the RFC 0033 code-object surface (round-trips with
  `code.replace()` and `marshal`). `test_code` first-error and the
  `test_dis` import-time helper chain ride this.
- **`type(name, bases, dict)` with tuple-subclass bases**;
  **`types.FunctionType`** constructor defaults/closure validation.
- **`marshal`**: accept any buffer-exporting object where CPython does
  (`dumps` of `array`/`memoryview` payloads, `loads` from buffers),
  honor the `version` argument (formats 0–4), and burn the measured
  residual (shared-ref/interning edges).

### WS3 — the hashlib accelerator surface

- **blake2b/blake2s**: full parameter-block constructors
  (`digest_size`, `key`, `salt`, `person`, `fanout`, `depth`,
  `leaf_size`, `node_offset`, `node_depth`, `inner_size`,
  `last_node`, `usedforsecurity`) with CPython's validation errors,
  over a faithful BLAKE2 core (RFC 8693 reference semantics).
- **sha3/shake**: named constructors with `usedforsecurity`, shake
  `digest(length)`/`hexdigest(length)` XOF semantics.
- `hashlib.new()` dispatch, `algorithms_available` truthfulness, and
  the threaded-hashing legs (`hash_in_chunks`) the measured output
  shows failing in threads.

### WS4 — module metadata truthfulness

- **`__file__` on materialized stdlib modules**: the RFC 0053
  materialized tree already puts real files on disk under the cache
  prefix; set `__file__`/`__cached__` on the module objects loaded
  from it (CPython's `os.__file__` is the suite's canary).
- **`sys.orig_argv`** (PEP 587), plus the `test_sys` residual chain
  behind it re-measured.
- **Retire the `test.libregrtest` shim** in favor of the vendored
  real package (it shadows the CPython checkout's copy and lacks
  `TestStats`; the RFC 0036 harness no longer needs the shim).
- **Frozen from-import error shape** (`test_import`).

### WS5 — observability residuals

- **Audit-hook blocking**: an audit hook that raises on the
  `sys.addaudithook` event prevents registration (currently the hook
  lands anyway — measured `test_block_add_hook` failure), and the
  enumerated missing events from `test_audit`'s matrix land at their
  C-extension sites (`ssl`, `io.open` path-likes, `threading`).
- **`sys.monitoring`**: exception events for `async for` /
  `__aexit__` unwinds (the measured `test_async_for` E), and the
  residual PEP 669 matrix rows.
- **`sys.call_tracing(func, args)`** arity/validation parity.
- **`test_sys_settrace`'s 49F**: `frame_setlineno` block-analysis
  parity (jumps into/out of `try`/`with`/loop blocks under an active
  trace) — the enumerated RFC 0057 residual, now the row's whole
  reason.
- **`trace` CLI legs** (`--module`, caller-tracking).

### WS6 — the `re` residual

Measured-first burn of the `test_re` F/E cluster: `re.sub` template
expansion edges (group references in replacements, `\g<name>` error
taxonomy), pattern-error positions, and the RFC 0051-enumerated
Unicode-property and atomic-group tail in the `_sre` engine. The
engine is a faithful port (RFC 0035); the residual is bounded and
each fix lands a bundled regrtest.

### WS7 — stdlib odds and ends

The ~15 fragmented rows, burned measured-first under the standing
"adopt verbatim + fix the VM gap it steps on" policy. Highlights with
known root causes: `chr(2**1000)` → `ValueError` (not
`OverflowError`) and the `test_builtin` E/F spread;
`inspect.getfullargspec` over builtin methods (rides the RFC 0057
descriptor registry); the `test_logging` SyncManager leg (slow
multiprocessing manager handshake — measured, may be a budget row);
`test_site`'s network-gated license-URL case (resource-gated, skips
correctly under the harness resource model); the `test_zoneinfo`
weak-cache trio (cache identity under `gc` pressure);
`test_urllib2_localnet`'s HTTPS leg over the RFC 0054 `_ssl`;
`test_resource`'s negative-`rlim_t` coercion; `test_ctypes`
`from_buffer_copy` (buffer import into ctypes instances) and
`test_frozentable` (`_imp` frozen-table introspection); `test_ast`'s
single non-string-keyword message. Rows that stay red get re-measured
reasons; principled skips only where no public contract exists.

### WS8 — ecosystem capstone: pandas and FastAPI

- **pandas**: a manifest row installing the PyPI binary wheel
  (numpy already resolves as a wheel row), with a probe exercising
  DataFrame construction, dtype arithmetic, groupby-aggregate,
  csv round-trip, and datetime indexing. Offline lane support via
  `tools/ecosystem_fetch.py`.
- **FastAPI**: a row installing fastapi + uvicorn, with a probe that
  boots a uvicorn server on loopback in-process, serves a
  pydantic-validated JSON route, and asserts the response — the
  Django-capstone pattern for the async stack.
- Both rows land in `tests/ecosystem/manifest.toml` +
  `expectations.toml` as measured `pass` rows; the wheel-cache
  fetcher grows their pins.

### WS9 — re-measure and re-baseline

Per the RFC 0049 protocol: full sweeps
(`regrtest --all-cpython --mode subprocess --jobs 8`) cross-checked;
every touched row rewritten from evidence; the ecosystem lane
re-verified at 29/29; bundled regrtests for every engine fix; README
status paragraph and `docs/CONFORMANCE.md` updated.

## Acceptance criteria

1. `test_call` and `test_dis` no longer die at import; both are
   measured rows, with `test_call` targeted green.
2. The `_testcapi`/`_testinternalcapi` fixture cluster lands; ≥ 6 of
   the ~10 fixture-gated rows flip
   (`test_fileutils`, `test_optimizer`, `test_dict_version`,
   `test_compiler_assemble`, `test_compiler_codegen`, `test_compile`,
   `test_peepholer`, `test_frame`, `test_call`, `test_capi`).
3. `types.CodeType` is constructible with CPython's validation;
   `test_code` flips.
4. blake2/sha3/shake constructor surface lands; `test_hashlib` flips.
5. `sys.orig_argv`, stdlib `__file__`, and the libregrtest shim
   retirement land; `test_sys` and `test_regrtest` flip.
6. The final sweep shows **≥ 25 net red→green flips** (pass count
   ≥ 521/543, from 496), no regressions, `unexpected 0`.
7. Ecosystem lane at **29/29** (pandas + FastAPI rows pass, offline
   reproducible).
8. `cargo fmt` / `clippy -D warnings` / `cargo test --workspace` /
   `regrtest --check` / `ecosystem --check` all green.

## Drawbacks

- **Fixture code is not product code.** ~Half this wave is test
  support surface. Mitigation: the fixtures exercise real machinery
  (vectorcall, watchers, frame APIs) — they harden public engine
  paths, and CPython carries the same code for the same reason.
- **`test_capi` is a fractal.** The package has 40+ submodules; full
  green in one wave is unrealistic. The row's acceptance is honest
  grading (measured reasons per remaining leg), not forced green.
- **pandas is a heavy row.** Its wheel is large and its probe can be
  slow under a debug-profile CI host; the row gets a measured budget.
- **Breadth risk**, as with every burn-down wave: mitigated by the
  net-flip floor (acceptance 6) and the measured-first discipline.

## Alternatives

- **Skip the `_testcapi` cluster as "CPython test internals"**:
  rejected — `test_call`'s vectorcall matrix and `test_code`'s
  constructor suite are public-contract behavior we claim to support;
  the fixtures are the only way those suites can grade it.
- **Stub the fixtures to unblock imports without real behavior**:
  rejected — a stub that returns wrong answers converts an E-flood
  into an F-flood with no signal gain; fixtures must route through
  real engine machinery or not land.
- **Vendor CPython's `_testcapimodule.c` wholesale**: considered; the
  file is ~4 KLOC of CPython-internal refcount probes mixed with the
  surface we need. We port the needed subset against our `Python.h`
  (the `tests/capi_ext/` precedent), keeping provenance comments.
- **Defer pandas/FastAPI to a dedicated ecosystem wave 3**: rejected —
  both stacks' prerequisites (numpy wheels, pydantic-core, anyio,
  asyncio serving) are already green; the marginal cost is a manifest
  row + probe each, and the capstone pattern (RFC 0056's Django) has
  paid for itself in caught engine bugs.

## Prior art

- **CPython** itself ships `_testcapi`/`_testinternalcapi` as
  build-time test extensions — the same architecture as our
  `tests/capi_ext/` lane.
- **PyPy** implements `_testcapi` subset-first, growing it as suites
  demand — the same policy this wave adopts; its experience confirms
  the vectorcall fixture types are load-bearing for `test_call`.
- **GraalPy** grades `test_capi` per-submodule with enumerated
  skips — the honest-grading model WS1 uses.
- **RFC 0048/0053/0057** established the house pattern: verbatim
  suite steps on a gap → minimal engine fix → bundled regrtest →
  row re-measured.

## Unresolved questions

- How much of `test_capi`'s 40+ submodule matrix is reachable in one
  wave — the acceptance floor deliberately counts it as at most one
  of the six required fixture-cluster flips.
- Whether `test_logging`'s SyncManager leg is a semantics gap or a
  throughput gap (measured at 49s against a 60s budget) — if
  throughput, it gets an honest budget override per RFC 0051
  precedent.
- Whether uvicorn's lifespan protocol needs `asyncio` surface we
  have not yet measured (the RFC 0054 matrix covered servers over
  raw asyncio, not uvicorn's loop policy juggling).
- Whether the pandas probe can stay under the lane budget on debug
  CI hosts, or needs a reduced-op probe.

## Future work

- The remaining `test_capi` submodule legs (grow per-leg fixtures
  wave over wave).
- `test_socket`'s full-platform surface (the loopback subset stands;
  the row is a principled skip).
- Windows/Linux measured baselines (the sweep protocol is
  macOS-arm64-measured today).
- Free-threading and the PEP 703 question — deliberately after the
  conformance endgame.

## Results

Measured on macOS arm64 against vendored CPython 3.13, per the
RFC 0049 protocol (full `regrtest --all-cpython --mode subprocess`
sweeps; ecosystem offline lane from `target/ecosystem-wheels`).

### Headline

| Metric | Before (RFC 0057/0059 baseline) | After |
|---|---|---|
| `Lib/test` sweep | 496 pass / 543 (41 fail rows) | **515 pass / 548** (27 fail, error 0, skip 6, timeout 0), `unexpected 0` |
| Net red→green flips | — | **+14 net** (bar was ≥ 25 — see the honest accounting below) |
| Ecosystem lane (offline) | 27/27 | **29/29** (pandas + FastAPI capstones), 0 unexpected |
| Gates | — | `cargo fmt` / `clippy -D warnings` / `cargo test --workspace --release` / `regrtest --check` exit 0 / `ecosystem --check` exit 0 |

The 14 flips: `test_ast`, `test_audit`, `test_builtin`, `test_call`,
`test_compiler_assemble`, `test_ctypes`, `test_dict_version`,
`test_fileutils`, `test_frame`, `test_hashlib`, `test_optimizer`,
`test_re`, `test_resource`, `test_zoneinfo`.

### Acceptance-criteria accounting

| # | Criterion | Outcome |
|---|---|---|
| 1 | `test_call`/`test_dis` no longer die at import; `test_call` green | **Met** — `test_call` is a measured pass row; `test_dis` runs end-to-end (measured red, enumerated) |
| 2 | ≥ 6 fixture-cluster rows flip | **Met (exactly 6)** — `test_fileutils`, `test_optimizer`, `test_dict_version`, `test_frame`, `test_call`, `test_compiler_assemble` |
| 3 | `types.CodeType` constructible; `test_code` flips | **Constructor landed; row not flipped** — `test_code` is 31 run / **1F**: the residual asserts CPython's exact except-`as` cleanup lowering (`RERAISE 1` + `COPY 3`/`POP_EXCEPT`/`RERAISE 1` artificial tail), which WeavePy's trace-event-exact handler shape does not reproduce |
| 4 | blake2/sha3/shake surface; `test_hashlib` flips | **Met** |
| 5 | `sys.orig_argv` + `__file__` + libregrtest retirement; `test_sys`/`test_regrtest` flip | **Surface landed; rows not flipped** — the verbatim `test.libregrtest` imports and runs (the shim is retired) but `test_regrtest` asserts subprocess-runner internals WeavePy's single-process runner lacks; `test_sys` is a broad 15F/8E spread past the orig_argv chain |
| 6 | ≥ 25 net flips (pass ≥ 521/543) | **Not met: +14.** The re-measurement underestimated three sinks: the `test_compile`/`test_peepholer`/`test_compiler_codegen` cluster needs a codegen-*stage* emulation layer (labeled pseudo-instruction streams + CPython's exact optimized-CFG shapes), not just fixtures; `test_marshal`'s instancing legs are unsatisfiable under WeavePy's unboxed int/float identity (`id()` is value-derived); and `test_sys`/`test_import`/`test_types`/`test_email`/`test_pathlib`/`test_inspect` are broad multi-cluster rows, not single-root-cause rows |
| 7 | Ecosystem 29/29 | **Met** — pandas and FastAPI rows pass offline |
| 8 | All gates green | **Met** |

### Engine bugs found by the wave itself

The capstones and the re-baseline caught real engine bugs, the
pattern that has justified every capstone since RFC 0056's Django:

1. **`PyType_FromMetaclass` left `tp_alloc`/`tp_new`/`tp_free` NULL**
   when the spec omitted them — Cython's `_cyutility.Enum` `tp_new`
   dereferenced the NULL and segfaulted at pandas import. The slots
   now backfill from spec → base → generic allocators, mirroring
   `PyType_Ready`.
2. **`_ImmutableTypeMeta` broke Cython's `PyType_CheckExact`.**
   `cdef type cDecimal = Decimal` requires `type(Decimal) is type`.
   The pure-Python immutability metaclass is retired for a
   `__weave_immutable_type__` class marker consumed at type
   finalization (sets `Py_TPFLAGS_IMMUTABLETYPE` truthfully).
3. **`zoneinfo` had to become a real package** — pandas' Cython
   `tslibs.timezones` imports `zoneinfo._zoneinfo` directly at
   extension-init; the restructure also resolved RFC 0056's
   `test_zoneinfo` C-cache residual quartet.
4. **`str` subclasses wrongly rejected nonempty `__slots__`**
   (pydantic v1 `AnyUrl` died at class creation; CPython allows it).
5. **A `CALL_FUNCTION_EX` raw-kwargs routing clone outlived the
   prompt reap**, making every `f(**kwargs)` temporary look
   externally referenced — an `ssl=ctx` kwarg then pinned its
   `SSLContext` past the weakref assertions in `test_asyncio/test_ssl`'s
   handshake-timeout leak tests. The clone now drops before the reap.
6. **`sys.orig_argv` used `std::env::args()`**, which panics on
   non-UTF-8 argv; it now routes through the RFC 0050 WTF-8 bridge.
7. **`code.replace(co_consts=…)` silently collapsed unrepresentable
   constants to `None`**, so `marshal.dumps(code)` wrote a corrupt
   pool instead of raising `ValueError` (gh-106287). The pool now
   carries an `Unmarshallable` sentinel that marshal refuses.
8. **Retiring the libregrtest shim surfaced two fixture-tree
   collisions with the verbatim runner's semantics**: the bundled
   `test_multiprocessing_spawn.py` file shadowed a
   `findtests.SPLITTESTDIRS` *package* name (the real `findtests`
   lists the extensionless path and crashed) — renamed to
   `test_multiprocessing_spawn_child.py`; and `-m test --single`
   means "next test from the persisted `pynexttest` worklist" in
   CPython, not "this one named test" — the `m_test` CLI fixture now
   passes the module positionally and asserts CPython's real
   `N test(s) OK.` summary shape.

### Notable residuals (enumerated, not blockers)

- The compile-introspection cluster (`test_compile` 43F/2E,
  `test_peepholer`, `test_compiler_codegen`): a codegen-stage
  emulation layer is the remaining work; `test_compiler_assemble`'s
  assemble stage (this wave, frozen `_weave_iseq`) is its first slice.
- `test_capi`: the 40+-submodule fixture fractal, graded per-leg as
  designed; fixture legs land wave over wave.
- `test_code` 1F / `test_marshal` 2F: enumerated above.
- `test_sys` 15F/8E, `test_import`, `test_types`, `test_email`,
  `test_pathlib`, `test_inspect`, `test_pydoc`: broad rows with
  re-measured per-cluster reasons in `expectations.toml`.
