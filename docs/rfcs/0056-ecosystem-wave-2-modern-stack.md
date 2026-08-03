# RFC 0056: Ecosystem wave 2 — the modern stack: a faithful `_sqlite3`, real expat, the ctypes/mock residuals, and a Django capstone

- **Status**: Accepted
- **Authors**: WeavePy authors
- **Created**: 2026-07-20
- **Tracking issue**: TBD
- **Builds on**: RFC 0055 (the ecosystem harness + venv/pip
  distribution surface this wave scales), RFC 0043–0047 (the binary
  ABI that makes wheel rows possible), RFC 0030 (in-tree pip; its
  PEP 425 tag matcher already prefers `cp313` > `abi3` > `none`),
  RFC 0022/0029 (extension loading, incl. `.abi3.so`), RFC 0049
  (measured whole-suite baseline protocol).

## Summary

RFC 0055 moved the acceptance bar from "passes CPython's tests" to
"runs real installed packages" and proved it for nine launch rows —
all pure-Python or incidentally-compiled (charset_normalizer's mypyc
`.so`). This wave scales that bar to **the packages that decide
whether a person can actually switch interpreters**: the web stack
(Flask, Django, SQLAlchemy, httpx), the data-model stack (pydantic
with its Rust `pydantic-core` wheel, PyYAML), the terminal stack
(rich, tqdm), and PyPI **binary wheels** consumed as-is (numpy,
markupsafe) instead of built from source.

The enabling work is exactly the largest remaining red clusters in
`tests/regrtest/expectations.toml`, so regrtest labels flip as a
side effect of ecosystem rows and vice versa:

1. **`_sqlite3`, for real.** Today's core is the 498-line RFC 0019
   shim: dict-shaped "objects", no exception hierarchy, no
   transactions, no `Row`, no `create_function`. Django's ORM,
   SQLAlchemy's dialect, `dbm.sqlite3`, and `test_sqlite3` (once its
   `load_package_tests` harness gap is closed) all sit on this one
   module. It gets the RFC 0048 treatment: a faithful native core
   over rusqlite's bundled SQLite, with CPython's `Lib/sqlite3/`
   package adopted verbatim on top.
2. **Real expat.** The pure-Rust `pyexpat` shim's measured gaps
   (utf-16 documents, entity declarations, unfired handlers) block
   `test_plistlib` (3 errors), `test_sax`, `test_minidom`,
   `test_pulldom`, and keep both `xml.etree` C-accelerator rows as
   principled skips. We vendor the real expat C library (the
   `vendor/lzma-sys` precedent) and rebuild `pyexpat` as bindings,
   the same "stop growing the shim" lesson every wave has re-taught.
3. **The ctypes and mock residual tails.** `test_ctypes` fails on
   callback-exception semantics over an otherwise-complete libffi
   bridge; `test_unittest`/`test_doctest`/`test_warnings` fail on VM
   introspection gaps (closure `cell_contents` mutation among them)
   under verbatim stdlib files. Both are measured-first residual
   burns, not rewrites — and both gate real rows, because every
   package's own test suite imports `unittest.mock`, and PyYAML's
   `libyaml` probe and cffi-adjacent packages route through ctypes.
4. **The capstone: Django.** A `django` ecosystem row that creates a
   project against the sqlite3 backend, runs `migrate`, exercises
   ORM CRUD + a queryset, and serves a request through the test
   client. When that row is green, "drop-in replacement" has its
   first full-framework proof.

As with every wave since RFC 0036, the deliverable is measured: the
ecosystem manifest grows from 9 rows to ~22 with every row graded
against a checked-in baseline (reds allowed, reasons mandatory), the
full regrtest sweep re-runs, and every touched row is rewritten from
evidence.

## Motivation

1. **The launch rows proved the harness, not the claim.** six and
   attrs are necessary but nobody migrates an interpreter for them.
   The README's promise is judged on `pip install django pydantic
   sqlalchemy` — the packages with C/Rust wheels, metaclass-heavy
   cores, and framework-scale stdlib demands. Each is either a green
   row or a measured reason after this wave.
2. **`_sqlite3` is the single highest-leverage module on the board.**
   It gates a stdlib module (`dbm.sqlite3`), two regrtest labels, the
   default backend of the most-deployed Python web framework, pip's
   own HTTP cache in upstream pip (named in RFC 0055's future work),
   and half the ORM ecosystem. The current shim cannot express a
   transaction. No other single artifact converts one implementation
   effort into as many measured flips.
3. **The XML family is one substrate.** Five red/skipped labels and
   the plistlib residuals share the expat shim as their root. CPython
   has never had a pure-Python expat; matching a C parser's event
   stream, buffer semantics, and error positions from a
   reimplementation is a losing game the codebase has already
   conceded twice (RFC 0048's shim lesson, RFC 0053's dual-truth
   lesson). Vendoring expat ends the class of bug.
4. **Binary wheels are the actual distribution mechanism.** RFC 0046
   built numpy *from source* to prove the ABI; real users run
   `pip install numpy` and get a manylinux/macosx wheel. The tag
   matcher and `.abi3.so` loader already exist; what's missing is a
   measured row proving the download-and-import path, and an audit of
   the PyO3/abi3 surface (`pydantic-core` is the forcing function —
   pydantic 2.x without it is a facade, and PyO3 is how the modern
   Rust-Python ecosystem ships).
5. **Cost of inaction.** The conformance tail elsewhere (numerics
   edges, `ast` fidelity) doesn't block adoption. A missing sqlite3
   and a pydantic that can't import do. Leaving them red keeps the
   ecosystem lane a demo instead of a gate.

## CPython reference

- `Modules/_sqlite/*.c` (`connection.c`, `cursor.c`, `statement.c`,
  `row.c`, `blob.c`, `module.c`, `microprotocols.c`) — the
  `_sqlite3` C extension: type layout, exception hierarchy
  (`Warning`/`Error`/`InterfaceError`/`DatabaseError`/`DataError`/
  `OperationalError`/`IntegrityError`/`InternalError`/
  `ProgrammingError`/`NotSupportedError`), statement cache,
  transaction control (`isolation_level`, 3.12+ `autocommit`),
  `detect_types` (`PARSE_DECLTYPES`/`PARSE_COLNAMES`), adapters and
  converters (`register_adapter`/`register_converter`,
  `PrepareProtocol`), `create_function(deterministic=)`,
  `create_aggregate`, `create_window_function`, `create_collation`,
  `set_authorizer`/`set_progress_handler`/`set_trace_callback`,
  `Connection.backup`, `serialize`/`deserialize`, `blobopen` +
  `Blob`, `Row` (sequence + mapping + `keys()`), `Cursor.description`
  7-tuples, `lastrowid`/`rowcount` exact semantics,
  `sqlite_version`/`version` module attrs, `threadsafety=3`.
- `Lib/sqlite3/{__init__,dbapi2,dump,__main__}.py` — adopted
  verbatim from `vendor/cpython/Lib/sqlite3/`.
- `Lib/dbm/sqlite3.py` — already verbatim; unblocks with the core.
- `Modules/pyexpat.c` + the bundled `Modules/expat/` — `xmlparser`
  attributes (`buffer_text`, `buffer_size`, `ordered_attributes`,
  `specified_attributes`, `namespace_prefixes`), the full handler
  table (19 handlers incl. `EntityDeclHandler`,
  `ExternalEntityRefHandler`, `SkippedEntityHandler`), `Parse`/
  `ParseFile`/`ExternalEntityParserCreate`/`SetParamEntityParsing`/
  `UseForeignDTD`, `ErrorString` + the `errors`/`model` submodules,
  `ExpatError` with `code`/`lineno`/`offset`,
  `CurrentLineNumber`/`CurrentColumnNumber`/`CurrentByteIndex`,
  and `_elementtree`'s `XMLParser` fast path over the same library.
- `Modules/_ctypes/callbacks.c` — callback exception handling: a
  Python exception raised inside a ctypes callback is written to
  `sys.unraisablehook` (`Exception ignored on calling ctypes callback
  function`) and the callback returns zeroed storage — it must not
  tear down the FFI call or leak into the caller.
- `Lib/unittest/mock.py`, `Lib/doctest.py` — already verbatim; the
  failures are VM gaps they step on (`cell.cell_contents` write
  support was added in CPython 3.7, `CodeType.replace` interactions,
  spec introspection over `__signature__`).
- PEP 384 / PEP 425 / PEP 597 (abi3, wheel tags); PyO3's
  `pyo3-ffi` limited-API import table is the practical abi3 symbol
  worklist for `pydantic-core`.
- Acceptance tests: `Lib/test/test_sqlite3` (package),
  `test_dbm_sqlite3.py`, `test_pyexpat.py`, `test_sax.py`,
  `test_minidom.py`, `test_pulldom.py`, `test_plistlib.py`
  (residuals), `test_xml_etree.py`/`test_xml_etree_c.py` (currently
  skipped), `test_ctypes.py`, `test_unittest.py`, `test_doctest.py`,
  `test_warnings.py`, `test_xmlrpc.py`.

## Detailed design

### WS1 — a faithful `_sqlite3` over rusqlite + verbatim `Lib/sqlite3`

Replace `stdlib/sqlite3_mod.rs` (498 lines) with a
`stdlib/sqlite3_native/` module family mirroring CPython's C layout:

- **`module.rs`** — module init: `connect()` (full keyword surface:
  `database`, `timeout`, `detect_types`, `isolation_level`,
  `check_same_thread`, `factory`, `cached_statements`, `uri`,
  `autocommit`), the ten-class exception hierarchy registered as
  real heap types (subclassing works; SQLAlchemy catches
  `sqlite3.IntegrityError`), `register_adapter`/`register_converter`
  process-global registries, `enable_callback_tracebacks`,
  `complete_statement`, `PARSE_DECLTYPES`/`PARSE_COLNAMES` +
  `SQLITE_*` authorizer/limit constants, truthful
  `sqlite_version`/`sqlite_version_info` from
  `rusqlite::version()`.
- **`connection.rs`** — a real `Connection` heap type. Transaction
  semantics ported from `connection.c`, not approximated: legacy
  `isolation_level` (implicit `BEGIN {DEFERRED,IMMEDIATE,EXCLUSIVE}`
  before DML when not in a transaction; `None` = autocommit) *and*
  3.12 `autocommit` (`LEGACY_TRANSACTION_CONTROL` sentinel,
  `True`/`False` modes), `in_transaction`, context manager
  commit/rollback. Callback surface: `create_function` (with
  `deterministic=True` → `SQLITE_DETERMINISTIC` — Django registers
  ~40 of these at connection setup), `create_aggregate`,
  `create_window_function`, `create_collation`, `set_authorizer`,
  `set_progress_handler`, `set_trace_callback` — all routed through
  rusqlite's hook APIs with Python exceptions converted per
  CPython's rules (`enable_callback_tracebacks` gating unraisable
  reporting). Plus `backup(target, *, pages, progress, name,
  sleep)`, `serialize`/`deserialize`, `blobopen` → `Blob` (seekable,
  buffer-protocol reads), `interrupt`, `total_changes`,
  `row_factory`, `text_factory`, `check_same_thread` enforcement
  (`ProgrammingError` from foreign threads), `close()` invalidating
  live cursors the way `pysqlite_check_connection` does.
- **`cursor.rs` + `statement.rs`** — `Cursor` as a real type with
  the iterator protocol, `execute`/`executemany`/`executescript`
  (script semantics: implicit commit first, run to completion,
  errors positioned), `description` 7-tuples with
  `PARSE_COLNAMES` column-name mangling (`"x [datetime]"`),
  converter application per `detect_types`, `lastrowid` (post-INSERT
  only, per `pysqlite_cursor` rules), `rowcount` (-1 for SELECT,
  accumulated for executemany), `arraysize`, `fetchone`/`fetchmany`/
  `fetchall` streaming from a live statement (not pre-materialized —
  `set_progress_handler` and `interrupt` must be observable
  mid-query), `row_factory` per-cursor override, and an LRU
  statement cache keyed by SQL text (`cached_statements`, default
  128).
- **`row.rs`** — `Row`: sequence + mapping (case-insensitive column
  lookup per `sqlite3_column_name` semantics), `keys()`, equality,
  hash.
- **Adapters/converters** exactly as `microprotocols.c`: adapt via
  registry then `__conform__(PrepareProtocol)`; the
  `dbapi2.register_adapters_and_converters()` datetime pair then
  behaves identically because the verbatim file registers them.
- **Verbatim adoption**: delete frozen `python/sqlite3.py`; adopt
  `sqlite3/{__init__,dbapi2,dump,__main__}.py` from the vendor tree
  into the `FrozenSource` table (materialized under `Lib/` per RFC
  0053). `dbm.sqlite3` re-measured.
- **Harness unblock**: `test.support.load_package_tests` (named by
  both the `test_sqlite3` and `test_zoneinfo` rows) lands in the
  `test.support` shim so package-style suites schedule their real
  tests.

Concurrency note: rusqlite `Connection` is `!Sync`; WeavePy heap
types are shared via the RFC 0025 cross-thread heap. The connection
is wrapped in the same single-owner mutex pattern `ssl_real.rs` uses
for rustls sessions, with `check_same_thread=True` (the default)
enforcing CPython's thread-affinity error before the lock is ever
contended, and `threadsafety = 3` truthful because the mutex
serializes cross-thread use when `check_same_thread=False`.

### WS2 — ctypes residual burn

`test_ctypes`'s measured first failure is callback exception
semantics. Port `callbacks.c` behavior: a Python exception inside a
`CFUNCTYPE` callback is routed to the unraisable hook with CPython's
exact message shape and the callback returns zeroed storage. Then
re-measure and burn the residual list (known suspects from the suite
map: `c_wchar_p`/`c_char_p` round-trips through `restype`,
`Structure` bitfields, `from_buffer` keepalive semantics,
`WINFUNCTYPE` absence on POSIX being an `AttributeError` not a crash,
`ctypes.util.find_library` on macOS 26's shared cache). PyYAML's
`libyaml` binding and the `cffi`-based rows below are the ecosystem
consumers; each residual fixed lands a bundled regrtest when it is an
engine behavior.

### WS3 — real expat: vendor the C library, rebuild `pyexpat`, unskip `_elementtree`

- **`vendor/expat/`** — expat 2.6.x, vendored with the `lzma-sys`
  discipline: upstream tree minus docs/tests/CLI, a thin `build.rs`
  compiling via `cc` with `XML_POOR_ENTROPY` off (we seed
  `XML_SetHashSalt` from `std`'s RNG), licenses preserved, a README
  pinning the upstream commit.
- **`stdlib/pyexpat_mod.rs` rebuilt as bindings** (the current
  1,349-line shim parser is deleted): `xmlparser` becomes a native
  heap type owning an `XML_Parser`, with the full attribute set
  (`buffer_text` coalescing, `buffer_size`, `ordered_attributes`,
  `specified_attributes`, `namespace_prefixes`, `intern`), all 19
  handler slots dispatching into Python callables (trampolines
  carry the GIL-held VM handle exactly like the sqlite3 hooks),
  `ExternalEntityParserCreate` sharing the parent's handler table,
  `ErrorString`/`error` codes verbatim, `ExpatError` carrying
  `code`/`lineno`/`offset`, and byte-exact
  `CurrentLineNumber`/`CurrentColumnNumber`/`CurrentByteIndex`.
  utf-16/latin-1/external-encoding documents now come free — expat
  does the decoding.
- **`_elementtree`**: with real expat under it, the
  `xml.etree.ElementTree` C-accelerator import path
  (`from _elementtree import *`) is implemented over the same
  bindings (TreeBuilder in Rust, `iterparse` incremental feed), and
  `test_xml_etree.py`/`test_xml_etree_c.py` graduate from principled
  skips to measured rows.
- Downstream re-measure: `test_sax`, `test_minidom`,
  `test_pulldom`, `test_plistlib` (its 3 expat-shim errors),
  `test_htmlparser` (shares the `_markupbase` substrate — measured,
  fixed if shallow), `test_xmlrpc` (its first failure is a
  `Fault.__dict__` shape bug in our `xmlrpc.client` shim — adopt
  verbatim if not already; measured either way), and `test_pyexpat`
  itself (its `_testcapi.set_nomemory` first failure gets the
  standing `_testcapi` stub treatment).

### WS4 — the mock/doctest/warnings introspection cluster

Measured-first: these three suites run under verbatim stdlib files,
so every failure is a VM gap. Known from the current rows:

- **Writable closure cells**: `cell.cell_contents` assignment +
  `del`, `cell.__eq__`, and constructing `types.CellType(value)` —
  `mock.patch` on closures and `test_warnings`' `@deprecated`
  retained-reference checks need them.
- **`unittest.mock` internals**: autospec walks
  `__signature__`/`__defaults__`/`__kwdefaults__`/`__wrapped__` and
  builds `_SpecState` lazily; the loader cluster in `test_unittest`
  suggests `TestLoader` path/discovery edges
  (`loader.discover` over namespace dirs, `__path__` shapes).
  Enumerate, fix, re-measure — the budget assumption is that this is
  ~a dozen object-model fixes, not a mock rewrite (mock.py is
  already verbatim).
- **doctest**: its row dies inside `unittest.suite`; likely the same
  loader substrate plus `linecache`/`__test__` discovery. Rides WS4;
  `test_zipimport_support`'s doctest-in-zip leg re-measured after.

Each engine fix lands a bundled regrtest
(`tests/regrtest/weavepy/…`) per standing policy.

### WS5 — ecosystem manifest wave 2 (~22 rows) + the abi3/PyO3 audit

New manifest rows, each with a behavior-asserting probe (not import
smoke). Grouped by what they prove:

| Group | Rows | Probe asserts |
|---|---|---|
| Web | `flask` (+werkzeug, itsdangerous, blinker) | route + test client request/response cycle, session cookie round-trip |
| | `sqlalchemy` | Core: table create/insert/select over sqlite3; ORM: declarative model + session commit + query |
| | `httpx` (+httpcore, anyio, sniffio, h11) | sync + async GET against local `http.server`, HTTPS against local rustls server |
| | `urllib3` (standalone) | pool manager GET, retry object, HTTPS |
| Data model | `pydantic` (+pydantic-core, annotated-types) | model validate/dump, field validators, `ValidationError` shape |
| | `pyyaml` | safe_load/safe_dump round-trip incl. anchors; C `_yaml` leg measured separately if the wheel carries it |
| | `msgpack` | pure-Python fallback pack/unpack round-trip |
| Terminal | `rich` (+markdown-it-py, pygments) | console render to captured buffer, table + traceback formatting |
| | `tqdm` | iteration wrapper over generator, postfix/format output shape |
| Binary wheels | `numpy` (PyPI wheel, not source build) | dtype arithmetic, broadcasting, `np.linalg.norm`, buffer round-trip via memoryview |
| | `markupsafe` (compiled wheel leg) | `_speedups` import + escape correctness |
| Infra | `certifi`, `idna`, `charset-normalizer` (standalone rows) | direct API probes (currently only exercised transitively) |
| Capstone | `django` (WS6) | below |

Stretch rows, landed as measured rows whatever their color:
`cryptography` (abi3 Rust wheel; Fernet round-trip + X.509 parse),
`orjson` (PyO3, version-specific wheel; dumps/loads round-trip),
`aiohttp` (C-accelerated multidict/yarl chain over RFC 0054
asyncio). A red row with a precise reason is a deliverable here —
it becomes the wave-3 worklist, per the RFC 0055 discipline.

**The abi3/PyO3 audit** is the engineering meat behind
`pydantic-core`/`cryptography`: enumerate the limited-API symbols
PyO3's `pyo3-ffi` import table actually binds (module-init via
`PyModuleDef` two-phase, `PyType_FromSpec` family — already live
since RFC 0028 — `PyObject_Vectorcall` under the abi3 spelling,
`PyGILState_*`, `PyInterpreterState_Get`, `Py_Enter/LeaveRecursiveCall`),
close the gaps in `weavepy-capi`, and add a bundled
`tests/capi_ext/_abi3check.c` fixture compiled with
`Py_LIMITED_API=0x030D0000` so the surface is regression-guarded
in-tree, independent of PyPI.

Runner changes are minimal: `manifest.toml` grows rows;
`tools/ecosystem_fetch.py` pins the new requirement set (exact `==`)
into the offline wheel cache including platform wheels for the
binary rows; the runner learns one new per-row key
(`needs_network = false` stays the default — probes use local
servers only).

### WS6 — the Django capstone row

- Row: `django` (pinned current LTS line), no extra deps beyond
  `asgiref`/`sqlparse`/`tzdata`.
- Probe (a real miniature project, not an import):
  `django-admin startproject` equivalent via
  `django.core.management`, `settings` on the sqlite3 backend,
  a one-model app, `makemigrations` + `migrate`, ORM CRUD +
  `filter().count()` + a transaction rollback via `atomic()`, then
  a request/response cycle through `django.test.Client` hitting a
  view that queries the model, asserting the rendered body.
- Expected engine demands beyond WS1: `functools.lru_cache`
  edge shapes, `asgiref.local.Local` over the RFC 0024 threading
  stack, `zoneinfo`/`tzdata` (the `test_zoneinfo` harness gap is
  fixed by WS1's `load_package_tests` port; the module itself
  shipped earlier), `logging.config.dictConfig`. Each is measured
  and either fixed in-wave or enumerated on the row.
- Stretch (not acceptance): running a slice of Django's own suite
  (`tests/runtests.py basic model_fields`) recorded as a
  `notes` field on the row.

### WS7 — re-measure and re-baseline

Per the RFC 0049 protocol: two full sweeps
(`regrtest --all-cpython --mode subprocess --jobs 8`) cross-checked;
every row this wave touches rewritten from evidence (`test_sqlite3`,
`test_dbm_sqlite3`, `test_zoneinfo`, `test_ctypes`, the XML family,
`test_unittest`, `test_doctest`, `test_warnings`, `test_xmlrpc`,
`test_secrets` if the WS2 residuals reach it); the ecosystem
baseline committed fully measured, offline lane verified from the
wheel cache. New bundled regrtests: sqlite3 transaction matrix
(isolation_level × autocommit × context manager), adapter/converter
round-trip, `Row` mapping semantics, callback-exception unraisable
shape (sqlite3 + ctypes), expat handler-table + utf-16 + external
entity fixtures, `_elementtree` iterparse, writable-cell semantics,
and the `_abi3check` limited-API fixture.

### Acceptance criteria

1. `_sqlite3` is the faithful core: the ten-class exception
   hierarchy, transactions (both `isolation_level` and
   `autocommit`), `Row`, `create_function(deterministic=)`,
   `create_aggregate`, `create_collation`, `set_authorizer`/
   `set_progress_handler`/`set_trace_callback`, `backup`,
   `blobopen`, `serialize`/`deserialize`; `Lib/sqlite3` is verbatim;
   `test_sqlite3` schedules its real package tests and its measured
   residual count is below 25 with every residual enumerated;
   `test_dbm_sqlite3` flips.
2. `vendor/expat` builds on the three CI platforms; `pyexpat` is
   bindings, not a shim; `test_pyexpat` moves past its first failure
   with residuals enumerated; `test_plistlib`'s 3 expat errors are
   gone; `test_sax`/`test_minidom`/`test_pulldom` flip or carry
   post-expat measured reasons; `test_xml_etree`/`test_xml_etree_c`
   are measured rows, not skips.
3. `test_ctypes` reaches a measured verdict past the callback
   first-failure with residuals below 15, enumerated.
4. Writable closure cells land with bundled regrtests;
   `test_warnings` flips; `test_unittest` and `test_doctest`
   residual counts drop by half or better, re-measured with honest
   reasons.
5. The ecosystem manifest carries ≥ 22 rows, all measured, offline
   lane green from the wheel cache; **at least 12 of the ~13 new
   non-stretch rows pass**, including `flask`, `sqlalchemy`,
   `pydantic` (with the real `pydantic-core` wheel), `pyyaml`,
   `httpx`, `numpy`-from-PyPI-wheel.
6. The `django` capstone row passes: migrate + ORM CRUD +
   transaction + test-client request against the WS1 sqlite3
   backend.
7. The `_abi3check` limited-API fixture compiles and runs in-tree.
8. At least 6 net regrtest labels flip red→green versus the wave-10
   baseline, no regressions, `unexpected 0` on the final sweep.
9. `cargo fmt` / `clippy -D warnings` / `cargo test --workspace` /
   `regrtest --check` / `ecosystem --check` all green.

## Drawbacks

- **Two more vendored C trees' worth of surface.** expat joins
  lzma-sys; SQLite is already bundled via rusqlite, but the new
  `_sqlite3` grows the native stdlib by an estimated 6–9 KLOC of
  Rust. Accepted: both replace shims whose bug classes were
  open-ended, and both are graded by upstream suites rather than
  our own beliefs.
- **Python-callable hooks from C callbacks** (sqlite3 functions,
  expat handlers, progress handlers) re-enter the VM from within a
  rusqlite/expat C frame. The GIL story (RFC 0024) must hold across
  the boundary; the trampoline pattern is proven by the RFC 0054
  `_asyncio`/`_ssl` callbacks, but sqlite3 window functions and
  authorizers add re-entrancy depth. Mitigation: the bundled
  transaction/callback regrtest matrix runs under the threaded
  regrtest mode.
- **PyPI drift on binary rows.** manylinux/macosx wheel pins are
  platform-conditional; the offline cache must carry per-platform
  wheels. Mitigated by exact pins per platform tag in
  `ecosystem_fetch.py` and the online lane staying non-blocking.
- **Scope risk on Django.** The ORM touches wide stdlib surface;
  a long tail could eat the wave. Mitigation: Django is the
  *capstone*, not the bulk — WS1–WS4 are independently valuable and
  independently measured; if the row lands red with a precise
  reason, the wave still ships (acceptance 6 is the goal; the
  fallback is a measured row plus enumerated engine gaps, explicitly
  flagged for wave 3).

## Alternatives

- **Load the host's `_sqlite3.cpython-313.so` through the binary
  ABI** instead of writing a native core: rejected — like `_ctypes`,
  `_sqlite3` is a core-built extension linked against private
  interpreter internals on several platforms, the host may not have
  it (or may link a different SQLite), and a drop-in interpreter
  cannot depend on a CPython installation being present. rusqlite's
  bundled, pinned SQLite gives identical behavior everywhere.
- **Grow the pure-Rust expat shim to cover utf-16 + entities**:
  rejected by the standing shim policy; the shim would still be
  chasing a C library's event-stream quirks (CDATA boundary events,
  buffer-split callbacks, error byte offsets) that the suites assert
  literally. Vendoring is a one-time cost that closes the class.
- **Bind a Rust XML crate (quick-xml) instead of C expat**:
  rejected — `pyexpat`'s public behavior *is* expat's, down to error
  codes and `ErrorString` text; emulating expat over a differently-
  shaped parser reintroduces the shim problem with extra steps.
- **Skip binary-wheel rows; keep building numpy from source**:
  rejected — source builds prove the ABI but not the distribution
  path users take; wheel rows also exercise the PEP 425 resolver and
  installer paths that RFC 0029/0030 built but nothing gates.
- **A Flask capstone instead of Django**: Flask is in the matrix,
  but as a capstone it under-tests — no ORM, no migrations, no
  bundled admin; Django's dependency on sqlite3 is exactly the
  forcing function WS1 needs.

## Prior art

- **CPython** bundles expat (`Modules/expat/`) and links SQLite
  system-or-bundled — precedent for both vendoring decisions,
  including the "the bundled copy is the spec" stance.
- **PyPy** reimplemented `_sqlite3` over its own FFI and passes the
  upstream `test_sqlite3` suite; its experience (statement-cache
  semantics and `lastrowid` edges were the long tail) informs WS1's
  test-first ordering. PyPy also ships real expat bindings rather
  than a reimplementation.
- **GraalPy** runs Django via its sqlite3 emulation and treats
  "Django tutorial works" as a headline compatibility milestone —
  the same capstone framing WS6 adopts.
- **PyO3/maturin** define the de-facto abi3 surface consumed by
  `pydantic-core`, `cryptography`, `orjson`; pyo3-ffi's
  limited-API bindings are a machine-checkable symbol worklist for
  the WS5 audit.
- **RFC 0048/0053** (shims re-diverge; adopt verbatim + native
  core), **RFC 0054** (C-callback trampolines under the GIL),
  **RFC 0055** (measured ecosystem lane) — the three house patterns
  this wave composes.

## Unresolved questions

- Whether `pydantic-core`'s current wheel line still ships abi3 or
  has moved to per-version cp313 wheels (both paths exist in the
  loader; the audit covers abi3 either way). Measured at
  implementation time and recorded on the row.
- Whether rusqlite's bundled SQLite version string matters to
  `test_sqlite3` (some cases gate on `sqlite_version_info >=`);
  if a case pins a *maximum*, the row records it rather than us
  downgrading.
- Whether `_elementtree` lands in-wave or as a fast-follow if the
  expat bindings consume the XML budget — `test_xml_etree_c`
  staying a skip one more wave is acceptable; the shim deletion is
  not.
- macOS/Windows CI wheel availability for the pinned binary rows
  (numpy/markupsafe publish broadly; `pydantic-core` for the CI
  macOS arch needs a pin check).

## Results

Measured on macOS arm64 against vendored CPython 3.13, per the
RFC 0049 protocol (two full `regrtest --all-cpython --mode
subprocess` sweeps; ecosystem offline lane from
`target/ecosystem-wheels`).

### Workstream outcomes

| WS | Deliverable | Result |
|---|---|---|
| WS1 | Faithful `_sqlite3` + verbatim `Lib/sqlite3` | `test_sqlite3` pass (504 run / 0 fail / 6 skip); `test_dbm_sqlite3` pass; `load_package_tests` unblocks package suites |
| WS2 | ctypes residual burn | `test_ctypes` past the callback first-failure; 1 enumerated residual (`test_frozentable`) |
| WS3 | Vendored expat + `pyexpat` bindings | `test_pyexpat`/`test_sax`/`test_minidom`/`test_pulldom`/`test_plistlib`/`test_htmlparser` pass; `test_xml_etree`/`test_xml_etree_c` measured pass (accelerator legs skip); `test_xmlrpc` 92/93 with one enumerated residual |
| WS4 | mock/doctest/warnings introspection | Writable cells + docs surface; `test_unittest`/`test_doctest`/`test_warnings`/`test_zipimport_support` pass |
| WS5 | Ecosystem wave-2 + abi3 audit | Manifest 9 → **27** rows, **all pass** offline; `_abi3check` limited-API fixture green in-tree |
| WS6 | Django capstone | pass — migrate + ORM CRUD + `atomic()` rollback + test-client request against WS1 sqlite3 |
| WS7 | Re-measure / re-baseline | Expectations rewritten from evidence; net red→green flips well above the ≥6 bar |

### Ecosystem matrix (offline lane)

27/27 pass, 0 unexpected — including every non-stretch row named in
acceptance 5 (`flask`, `sqlalchemy`, `pydantic`+`pydantic-core`,
`pyyaml`, `httpx`, `numpy` from PyPI wheel) **and** the stretch set
(`cryptography`, `orjson`, `aiohttp`) **and** the Django capstone.

### Notable residuals (enumerated, not blockers)

- `test_extcall`: ~24 doctest Failed examples — duplicate-`**` keyword
  detection, `*`/`**` unpack TypeError spelling, builtin `dir()`
  message fidelity. Kwonly too-many-positionals shape fixed in-wave
  (bundled `test_extcall_kwonly_messages.py`).
- `test_pyclbr`: `cm('pickle')` needs a native `_pickle` so
  `Pickler.__module__ == '_pickle'`.
- `test_zoneinfo`: 4 C-extension-implementation-detail residuals
  (weak-cache corruption trio + `ExtensionBuiltTest.test_cache_location`).
- `test_xmlrpc`: `Fault.__dict__` still surfaces the `args`
  pseudo-slot (exception slot-storage follow-up).
- `test_ctypes`: `test_frozentable` needs real frozen-module C tables.

### Acceptance checklist

1. `_sqlite3` faithful core + verbatim package — **met**.
2. Vendored expat / `pyexpat` bindings / XML family measured — **met**.
3. `test_ctypes` past callback first-failure, residuals enumerated — **met**.
4. Writable cells; `test_warnings`/`test_unittest`/`test_doctest` — **met**.
5. ≥22 ecosystem rows, ≥12 new non-stretch green — **met** (27/27).
6. Django capstone — **met**.
7. `_abi3check` fixture — **met**.
8. ≥6 net regrtest flips, `unexpected 0` on the final sweep — **met**
   (14 red→green vs wave-10 baseline; final sweep
   `542 total — pass 418 / fail 113 / skip 8 / timeout 3 — unexpected 0`).
9. `cargo fmt` / `clippy -D warnings` / `cargo test --workspace` /
   `regrtest --check` / `ecosystem --check` — **met**.

## Future work

- **Upstream pip as the bundled wheel** (RFC 0055 future work) —
  unblocked by WS1, since upstream pip's HTTP cache imports
  `sqlite3`.
- **`_decimal`** — `test_decimal` stays a principled skip this
  wave; a native decimal core is its own wave-sized artifact.
- **Ecosystem wave 3**: the reds this wave's stretch rows surface
  (aiohttp's C chain, cryptography if gaps remain), plus
  upstream-test-suite rows (`pytest` running Flask's own tests) and
  a scientific-stack row set (scipy, matplotlib) over the binary
  ABI.
- **Windows lane** for the ecosystem harness (activation scripts +
  platform wheels), carried over from RFC 0055.
