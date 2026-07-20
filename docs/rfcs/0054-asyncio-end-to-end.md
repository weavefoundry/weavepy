# RFC 0054: The async wave — native `_asyncio`, OpenSSL-shaped `_ssl`, and retiring the network skips

- **Status**: Accepted
- **Authors**: WeavePy authors
- **Created**: 2026-07-15
- **Tracking issue**: TBD
- **Builds on**: RFC 0053 (wave 8 — source-truth stdlib; its future-work
  section names this wave: "asyncio end-to-end (native `_asyncio`,
  subprocess/SSL transports, per-submodule grading of the 31-module
  package) — the largest remaining skip"), RFC 0042 (networking and web
  stack: `_socket`, `_ssl`-over-rustls, `select`), RFC 0039 (concurrency
  wave 4: selector backends + `SelectorEventLoop`), RFC 0024/0025
  (threads + GIL + cross-thread heap), RFC 0049 (measured whole-suite
  baseline protocol).

## Summary

`test_asyncio` is the largest remaining `skip` row in
`tests/regrtest/expectations.toml` — a 31-submodule, ~29,000-line
package graded as a single label that the harness cannot mark green as
a unit. Four more network suites sit in the same deferred bucket:
`test_ssl` (5,635 lines), `test_socket` (7,277 lines), `test_urllib2`,
and `test_poplib`. Together these five rows are the gap between
"passes regrtest percentages" and "runs the modern Python ecosystem":
FastAPI, uvicorn, httpx, aiohttp, and half of server-side PyPI are
asyncio-first, and every one of them needs a working event loop,
working TLS-over-memory-BIO, and a C-accelerator-shaped `_asyncio`.

A measured pre-wave baseline (running submodules directly under the
current tree) shows the pure-Python loop is already substantially
alive — `test_locks` 75/75, `test_queues` 59/59,
`test_selector_events` 129 run / 4 errors, `test_subprocess` 158 run /
9 errors — while roughly half of `test_futures`' 301 tests skip for
want of the C accelerator (`_CFuture`/`_CTask` parametrizations), and
four submodules (`test_tasks`, `test_streams`, `test_events`,
`test_sslproto`) hang outright.

Wave 9 makes async real, in five workstreams:

1. **Native `_asyncio`.** A new `asyncio_mod.rs` implementing
   CPython's `Modules/_asynciomodule.c` surface: `Future`, `Task`
   (as real native types with the documented C semantics —
   `__schedule_callbacks`, eager `__step` scheduling,
   cancellation-count bookkeeping), `get_event_loop` /
   `get_running_loop` / `_get_running_loop` / `_set_running_loop`,
   `current_task` / `all_tasks`, the per-loop running-task registry
   (`_register_task` / `_unregister_task` / `_enter_task` /
   `_leave_task` / `_swap_current_task`), and the
   `future_add_done_callback` fast path. The frozen
   `asyncio/futures.py` and `asyncio/tasks.py` already contain
   CPython's `try: import _asyncio` adoption hooks — after this
   workstream they bind the C variants exactly as CPython does, and
   the ~120 currently-skipped `C*` parametrizations in
   `test_futures`/`test_tasks`/`test_taskgroups`/… become live tests.
2. **The transport layer, measured and de-hung.** The pure-Python
   `SelectorEventLoop` over native `select`/`_socket` already carries
   TCP/UDP/pipe/subprocess transports; this workstream fixes what the
   baseline measured as broken rather than rebuilding what works. The
   known blockers: `recv_into` rejecting non-`bytearray` writable
   buffers (measured: it breaks `sslproto`'s `memoryview`-slice reads
   and any protocol using `BufferedProtocol`), the hangs in
   `test_tasks`/`test_streams`/`test_events`/`test_sslproto`
   (diagnosed and fixed individually — hang class, not test class),
   signal-handler integration on the loop (`loop.add_signal_handler`
   over the existing `signal.set_wakeup_fd`), and
   `loop.sendfile`/`sock_sendfile` over native `os.sendfile` where
   the platform has it.
3. **OpenSSL-shaped `_ssl`.** The rustls core (RFC 0042) already
   exposes the MemoryBIO/`wrap_bio` path `sslproto` needs. What is
   missing is the OpenSSL-visible surface the test suites and real
   packages probe: `getpeercert()` full X.509→dict parsing (subject /
   issuer RDN sequences, SANs, notBefore/notAfter, serialNumber,
   version, OCSP/caIssuers), `SSLContext.set_servername_callback` /
   `sni_callback` actually firing during server handshakes,
   `SSLContext.options` / `verify_flags` wired into the rustls config
   where representable (TLS-version floors/ceilings, cipher policy)
   and faithfully stored/reported where not, `cert_store_stats()` /
   `get_ca_certs()`, `SSLSocket.shared_ciphers()` /
   `get_channel_binding()`, session objects with CPython's attribute
   shape, and CPython-shaped `ssl.SSLError` subclasses
   (`SSLWantReadError`, `SSLWantWriteError`, `SSLSyscallError`,
   `SSLEOFError`, `SSLCertVerificationError` with
   `verify_code`/`verify_message`). `ssl.py` moves from the 1,304-line
   shim toward CPython 3.13's file, adopted verbatim-where-possible
   per the RFC 0048 policy, with a small documented divergence list
   for rustls-vs-OpenSSL representational gaps.
4. **Per-submodule grading of `test_asyncio`.** The harness
   (`weavepy-conformance regrtest`) learns to expand test *packages*
   into one label per submodule
   (`cpython/Lib/test/test_asyncio/test_futures.py`, …), so each of
   the 31 files gets its own measured row, timeout, and reason —
   the same granularity CPython's own regrtest reports. The
   monolithic package label is retired. Package expansion is generic:
   `test_json`, `test_sqlite3`, `test_zoneinfo`, and friends ride the
   same mechanism (their `load_package_tests` harness gap is one of
   this wave's measured fixes if shallow, or an enumerated row if
   not).
5. **Measured rows for the network skips.** `test_ssl`,
   `test_socket`, `test_urllib2`, and `test_poplib` graduate from
   `skip` to measured rows under the loopback-only policy: suites run
   with `test.support`'s network-resource gate disabled (the sweep
   sandbox has no internet), hangs are fixed or the specific hanging
   cases enumerated, and every residual failure carries a
   first-failure reason. The IPv6 hang in `test_socket` and the
   parallel-load flake in `test_poplib` recorded by earlier waves are
   in scope as diagnosable bugs, not re-skips.

As with every wave since RFC 0036, the deliverable is *measured*: the
full sweep re-runs, `tests/regrtest/expectations.toml` is rewritten
from evidence (now with per-submodule asyncio rows), and every red row
carries an actionable first-failure reason.

## Motivation

1. **The drop-in claim is hollow without asyncio.** The README grades
   WeavePy by whether existing code runs unchanged. In 2026 "existing
   code" means `async def`: FastAPI + uvicorn, httpx, aiohttp,
   websockets, asyncpg. All of them import `asyncio` at startup and
   most touch `sslproto` (TLS client/server) and the subprocess
   transports within their test suites. This is the single largest
   ecosystem gate left.
2. **Half the asyncio suite is skipped for one missing module.**
   CPython parametrizes its asyncio tests over the pure-Python and C
   implementations (`PyFutureTests` vs `CFutureTests`, …). Without
   `_asyncio`, 120 of `test_futures`' 301 tests skip, and equivalents
   across `test_tasks`, `test_taskgroups`, `test_eager_task_factory`
   (which is *entirely* about `_asyncio`-backed eager tasks),
   `test_futures2`, and `test_context` never run. One native module
   converts hundreds of skips into live conformance signal.
3. **The C accelerator is semantically load-bearing, not just fast.**
   CPython's `_asyncio.Task` has observable differences the suite
   probes: eager task factories (`asyncio.eager_task_factory`),
   `Task.cancelling()` / `uncancel()` bookkeeping, the
   linked-list task registry behind `all_tasks()`, and traceback
   shapes. The frozen `tasks.py` currently carries a WeavePy-local
   `_strip_step_frame` patch to imitate C tracebacks — a shim this
   wave deletes in favor of the real mechanism.
4. **`sslproto` is the highest-value TLS consumer and it is blocked
   on measured bugs.** The MemoryBIO plumbing exists (RFC 0042); the
   probe that fails is `recv_into(memoryview)` — an engine-level
   buffer-protocol gap, exactly the class of shallow bug the wave
   protocol exists to flush out. Meanwhile `ssl.py`'s stub
   `getpeercert() -> {}` silently breaks every hostname-verification
   code path that inspects the peer certificate — including
   `aiohttp`'s and `httpx`'s error reporting.
5. **One label hides thirty-one measurements.** The whole-package
   `test_asyncio` row cannot distinguish "the locks suite is 75/75"
   from "test_events hangs". Wave 5 (RFC 0049) established that
   scope mechanisms which hide measurement get retired; per-submodule
   expansion is the same move at package granularity, and it benefits
   every other vendored test package.
6. **Cost of inaction.** Every future wave that touches networking,
   HTTP, or subprocess pipes re-discovers these hangs from scratch,
   and the top-of-funnel ecosystem proof (`pip install fastapi` and
   run its tests) stays blocked behind an unmeasured monolith.

## CPython reference

- `Modules/_asynciomodule.c` — `Future`/`Task` C implementation:
  `FutureObj`/`TaskObj` layout, `future_schedule_callbacks`,
  `task_step_impl`, the eager-task path (`task_eager_start`), the
  `all_tasks` linked list + WeakSet split, `swap_current_task`,
  module-state loop caching (`cached_running_loop`).
- `Lib/asyncio/futures.py` / `tasks.py` / `events.py` — the
  `try: from _asyncio import …` adoption sites (already frozen
  verbatim in WeavePy).
- `Lib/asyncio/sslproto.py` — `SSLProtocol` over `MemoryBIO` +
  `SSLObject`: `do_handshake`, `feed_ssldata`/`feed_appdata` flow
  control, `_SSLProtocolTransport`.
- `Lib/asyncio/selector_events.py` — `_SelectorSocketTransport`
  (`recv_into` into `get_buffer()` results — the `BufferedProtocol`
  path), `_SelectorDatagramTransport`, `sock_sendfile`.
- `Lib/asyncio/unix_events.py` — `_UnixSelectorEventLoop`
  (`add_signal_handler` over `signal.set_wakeup_fd`), child watchers,
  `_UnixReadPipeTransport`/`_UnixWritePipeTransport`,
  `_UnixSubprocessTransport`.
- `Modules/_ssl.c` — `_decode_certificate` (the X.509→dict shape:
  `subject`/`issuer` as RDN tuple-of-tuples, `subjectAltName`,
  `notBefore`/`notAfter` in ASN.1 GENERALIZEDTIME text form,
  `serialNumber` uppercase hex, `OCSP`/`caIssuers`),
  `_servername_callback` (SNI dispatch: callback gets
  `(SSLObject/SSLSocket, servername_or_None, SSLContext)`),
  exception hierarchy construction, `SSLSession` attributes
  (`id`, `time`, `timeout`, `ticket_lifetime_hint`, `has_ticket`).
- `Lib/ssl.py` (3.13) — the file the shim converges toward:
  `SSLContext` public surface, `Purpose`, `create_default_context`,
  `match_hostname` removal state, `SSLSocket._create` flow.
- Acceptance tests: `Lib/test/test_asyncio/` (31 submodules),
  `test_ssl.py`, `test_socket.py`, `test_urllib2.py`,
  `test_poplib.py`, plus the C-parametrized halves of the asyncio
  suite as the `_asyncio` acceptance harness.

## Measured pre-wave baseline

Run directly under the current tree (debug build, 120 s cap per
submodule, vendored CPython 3.13 `Lib/test/`):

| Submodule | Result |
|---|---|
| `test_futures` | 181 run, 1 error, **120 skipped** (C-accel) |
| `test_locks` | 75 run, clean |
| `test_queues` | 59 run, clean |
| `test_base_events` | 113 run, 2 failures, 8 errors |
| `test_subprocess` | 158 run, 9 errors, 45 skipped |
| `test_unix_events` | 125 run, 5 errors, 10 skipped |
| `test_selector_events` | 129 run, 4 errors |
| `test_tasks`, `test_streams`, `test_events`, `test_sslproto` | **hang** (>120 s) |

TLS-over-asyncio probe (`asyncio.start_server(ssl=…)` +
`open_connection(ssl=…)`): handshake path reaches
`_read_ready__get_buffer` and dies on
`TypeError: recv_into expects a bytearray` — the engine accepts only
`bytearray`, not the `memoryview` slices `sslproto` passes.

Plain-TCP echo (streams API), `asyncio.run`, `asyncio.sleep`, and
`create_subprocess_exec` + `communicate()` all work today.

## Detailed design

### WS1 — native `_asyncio` (`asyncio_mod.rs`)

A new `crates/weavepy-vm/src/stdlib/asyncio_mod.rs`, registered as
`_asyncio` in `stdlib::register_all`, following the
`socket_mod.rs` native-class pattern (`TypeObject::new_user` +
builtin methods + getsets via `descr_registry`).

**`_asyncio.Future`.** Native type with CPython's slot layout held in
a Rust struct behind the instance (state enum
`PENDING`/`CANCELLED`/`FINISHED`, result/exception slots, callbacks
as `(callback, context)` pairs with the 1-callback inline fast path,
`_asyncio_future_blocking`, loop reference, `source_traceback`,
`cancel_message`). Methods: `result`, `set_result`, `exception`,
`set_exception`, `add_done_callback`, `remove_done_callback`,
`cancel(msg=None)`, `cancelled`, `done`, `get_loop`, `__await__` /
`__iter__` (the two-step yield-self protocol), class getsets
(`_state`, `_callbacks`, `_result`, `_exception`, `_loop`,
`_source_traceback`, `_cancel_message`), `__init__(*, loop=None)`
with the `get_event_loop` fallback, subclassable (`__init_subclass__`
compatible; `test_futures` subclasses it), `__del__` un-retrieved
exception logging via the loop's `call_exception_handler`.

**`_asyncio.Task(Future)`.** Adds the step machinery: `__init__`
(coro validation, `set_name`, eager_start kwarg, context capture via
`contextvars.copy_context`), `__step`/`__step_run_and_handle`
implemented in Rust driving `coro.send`/`throw` with
CPython-equivalent scheduling (`loop.call_soon` with context),
result-of-yield dispatch (future-blocking flag protocol, bare-yield
rejection, `StopIteration` result capture), cancellation
(`cancel(msg)` requesting-state, `cancelling()`, `uncancel()`,
`_must_cancel` deferred delivery), `get_stack`/`print_stack` (over
the frame surface RFC 0033 built), `get_coro`, `get_context`,
`get_name`/`set_name`, and eager start (`eager_start=True` runs the
first step synchronously and only schedules if the coro yields — the
`asyncio.eager_task_factory` contract `test_eager_task_factory`
probes).

**Module functions + registry.** `get_event_loop`,
`get_running_loop`, `_get_running_loop`, `_set_running_loop`
(thread-local slot in Rust, shared with the frozen `events.py` via
the adoption hook), `current_task(loop=None)`, `all_tasks(loop=None)`,
`_register_task`/`_unregister_task` (the WeakSet for non-native
tasks), `_register_eager_task`/`_unregister_eager_task`,
`_enter_task`/`_leave_task`/`_swap_current_task`, `future_add_to_awaited_by` /
`future_discard_from_awaited_by` if the 3.13 surface carries them
(match the vendored `futures.py` import list exactly — whatever names
the frozen files import must exist).

**Shim retirement.** The WeavePy-local `_strip_step_frame` patch in
the frozen `tasks.py` is deleted; `tasks.py` returns to byte-verbatim
CPython 3.13. `test_asyncio/test_futures.py`'s
`CFutureTests`/`CSubFutureTests` and `test_tasks.py`'s C
parametrizations become the acceptance harness for this workstream —
they compare C and Python implementations against each other in the
same process, which is the strongest cross-check available.

### WS2 — transports: fix what the baseline measured

- **`recv_into` buffer protocol.** `_socket.socket.recv_into` (and
  `recv_from_into`, and the `readinto` paths that share the helper)
  accept any object exporting a writable buffer — `memoryview`,
  `array`, numpy arrays via the RFC 0028 buffer protocol — not just
  `bytearray`. This unblocks `BufferedProtocol` and `sslproto`.
- **Hang diagnosis.** Each of the four hanging submodules is run
  under a per-test watchdog to identify the specific hanging tests;
  fixes land per root cause. Known candidate classes from prior
  waves: missing loop-wakeup on cross-thread `call_soon_threadsafe`,
  `run_until_complete` re-entrancy, blocking `getaddrinfo` on
  loopback names, and tests that require a functioning
  `loop.add_signal_handler`. Whatever cannot be fixed in-budget is
  enumerated per-test in the (now per-submodule) expectations rows —
  a hang converts to a measured row either way.
- **Signal integration.** `_UnixSelectorEventLoop.add_signal_handler`
  requires `signal.set_wakeup_fd` (present per RFC 0040) plus
  `signal.siginterrupt` and warn-on-full-pipe semantics; wire and
  test via `test_unix_events`' signal cases.
- **`sock_sendfile` / `loop.sendfile`.** Native `os.sendfile` on
  macOS/Linux with the CPython fallback protocol
  (`SendfileNotAvailableError` → user-level copy);
  `test_asyncio/test_sendfile.py` is the acceptance suite.
- **Datagram + pipe transports.** Already present in the frozen
  package; the baseline's `test_unix_events`/`test_selector_events`
  errors (9 total) are triaged in-wave — they are the residual, not
  the architecture.

### WS3 — OpenSSL-shaped `_ssl`

The rustls core stays; the OpenSSL-visible *shape* becomes faithful.

- **X.509 → dict.** A DER parser (via the already-vendored
  `x509-parser`-class machinery in the rustls dependency tree, or a
  small hand-rolled DER walker if the dependency surface is not worth
  it) producing CPython's exact `getpeercert()` dict: RDN
  tuple-of-tuples for `subject`/`issuer`, `subjectAltName` pairs
  (`DNS`/`IP Address`/`email`), `notBefore`/`notAfter` in OpenSSL's
  `"%b %d %H:%M:%S %Y GMT"` text form, uppercase-hex
  `serialNumber`, integer `version`, `OCSP`/`caIssuers` URI tuples.
  `binary_form=True` keeps returning DER (already works).
- **SNI callbacks.** rustls server config gains a
  `ResolvesServerCert` implementation that captures the received SNI,
  parks the handshake, and dispatches the Python
  `sni_callback(sslobj, servername, context)` on the VM thread before
  resuming with the (possibly swapped) certificate/context. Callback
  exceptions map to the ALERT the suite expects
  (`SSLError` with handshake failure on the client side).
- **Exception hierarchy.** `ssl.SSLError` (aliasing `_ssl.SSLError`,
  an `OSError` subclass) with `library`/`reason` attributes, and the
  `SSLWantReadError` / `SSLWantWriteError` / `SSLSyscallError` /
  `SSLZeroReturnError` / `SSLEOFError` /
  `SSLCertVerificationError(verify_code, verify_message)` subclasses,
  raised from the Rust core with CPython's `reason` strings where
  rustls exposes the distinction (`CERTIFICATE_VERIFY_FAILED`,
  `WRONG_VERSION_NUMBER`, `UNEXPECTED_EOF_WHILE_READING`, …).
- **Context knobs.** `minimum_version`/`maximum_version` +
  `OP_NO_TLSv1_*` wired to rustls protocol-version selection;
  `verify_mode`/`check_hostname` interlock semantics
  (`ValueError: check_hostname requires verify_mode != CERT_NONE`);
  `options` default including `OP_NO_COMPRESSION | OP_CIPHER_SERVER_PREFERENCE
  | OP_ENABLE_MIDDLEBOX_COMPAT`-shaped bits so equality tests read
  CPython-plausible values; `cert_store_stats()`, `get_ca_certs()`
  (dict form via the WS3 X.509 decoder), `set_default_verify_paths`,
  `load_default_certs(purpose)`. Knobs rustls cannot honor
  (`set_ecdh_curve`, `load_dh_params`, cipher-string grammar beyond
  filtering the rustls suite list) store-and-report faithfully and
  are enumerated in the module docstring + the divergence list.
- **`ssl.py` convergence.** The shim moves toward verbatim CPython
  3.13 `ssl.py`, keeping a minimal `_ssl`-adapter delta. Every
  intentional divergence gets a `# WeavePy:` comment and a row in
  `docs/CONFORMANCE.md`'s divergence table.

### WS4 — per-submodule package grading in the harness

In `crates/weavepy-conformance/src/regrtest.rs`:

- Discovery: when a `test_*/` package directory contains its own
  `test_*.py` files, schedule one label per submodule —
  `cpython/Lib/test/test_asyncio/test_futures.py` — instead of (not
  in addition to) the package label. Packages without test-file
  children (or with `load_tests` magic that composes differently,
  e.g. parametrizing one module across C/pure variants like
  `test_json`) keep the package label; the expansion is driven by a
  file-shape check, not a hardcoded list.
- The bootstrap imports `test.test_asyncio.test_futures` and loads
  tests from that module only; per-row `timeout_seconds` applies per
  submodule (default 60 s, so one hanging file no longer poisons 30
  green ones).
- `expectations.toml` grows the per-submodule rows; the
  `--check` sweep, `--all-cpython` accounting, and the README label
  counts follow automatically since rows are just labels.
- The `load_package_tests` harness gap recorded on
  `test_sqlite3`/`test_zoneinfo`/`test_dbm_sqlite3` is re-measured
  under the new mechanism; if the residual is the discovery shape
  itself, this workstream fixes it for free.

### WS5 — measured rows for the network skips

Policy: the sweep runs loopback-only (`test.support.requires('network')`
resources stay un-granted, exactly like CPython's `-u-network`
default), so these suites are gradeable without internet.

- **`test_ssl`**: re-run under the WS3 surface. The previously
  recorded hangs are diagnosed (the suite's threaded echo servers
  exercise blocking handshakes with timeouts — the non-blocking
  handshake path must honor `settimeout`). Residual
  rustls-representational failures (cipher-string grammar,
  `session_stats`) are enumerated per test in the row reason.
- **`test_socket`**: the IPv6 hang is root-caused (suspect:
  `getaddrinfo`/dual-stack loopback binding); `sendmsg`/`recvmsg`
  ancillary-data cases re-measured (`SCM_RIGHTS` shipped in RFC
  0040). SCTP and platform-absent families skip via the suite's own
  guards.
- **`test_urllib2`**: predominantly a mock-based suite (no live
  network needed for most of it); runs under
  `requires_subprocess` support which RFC 0040 provides. Measured.
- **`test_poplib`**: the recorded parallel-load flake gets a real
  diagnosis (suspect: accept-backlog race in the threaded test
  server against the non-blocking connect path). If the fix is in
  WeavePy, land it; if the flake reproduces on CPython under equal
  load, record that finding and mark the row pass-with-note.
- `test_decimal`/`test_xml_etree` (the other big skips) stay out of
  scope: `_decimal` and `_elementtree` are their own accelerators
  with no asyncio coupling — recorded as candidates for wave 10.

### WS6 — long-tail engine salt

Fixed alongside, each with a bundled regrtest where the fix is in the
engine rather than a stdlib file:

- `recv_into`-class writable-buffer acceptance anywhere the native
  IO stack takes a buffer argument (`recv_into`, `recvfrom_into`,
  `readinto`, `os.readv`-style paths) — one shared helper.
- Whatever the four hang diagnoses turn up (each fix is a WS6 item
  with a fixture if it is engine-level, e.g. eval-breaker wakeups on
  `call_soon_threadsafe`).
- `builtin '__new__' does not accept keyword arguments` — the
  measured `test_futures` error: `cls.__new__(cls, loop=x)` on a
  native type must accept-and-ignore excess kwargs exactly when
  `__init__` is overridden (CPython's `object.__new__` rules applied
  to native types with default `tp_new`).
- Anything the C/Python cross-parametrized suites flush out of the
  new native types (equality, `__class_getitem__` on
  `Future`/`Task` — both are generic in 3.13, `repr` shapes).

### WS7 — re-measure and re-baseline

Per the RFC 0049 protocol: two full sweeps
(`--mode subprocess --jobs 8`), cross-checked; `expectations.toml`
rewritten with the per-submodule asyncio rows and the graduated
network rows; every red row carries a measured first-failure reason;
README status paragraph updated. New bundled regrtests: `_asyncio`
C/Python parity fixtures (future state machine, task cancellation
bookkeeping, eager start), an end-to-end asyncio TLS echo
(client+server over `sslproto` on loopback), `getpeercert()` dict
shape against a fixture certificate, SNI callback dispatch, and the
WS6 fixtures.

### Acceptance criteria

1. `import _asyncio` succeeds and the frozen `futures.py`/`tasks.py`
   adoption hooks bind it: `asyncio.Future is _asyncio.Future`,
   `asyncio.Task is _asyncio.Task`, and
   `asyncio.eager_task_factory` exists and works.
2. The C-parametrized halves of `test_futures`, `test_tasks`,
   `test_taskgroups`, and `test_eager_task_factory` run (not skip);
   `test_futures` ≥ 295/301 with zero errors.
3. None of the 31 `test_asyncio` submodules hangs the sweep: every
   row is `pass` or a measured `fail` with an enumerated reason;
   at least 20 of 31 submodule rows are `pass`, including
   `test_locks`, `test_queues`, `test_futures`, `test_tasks`,
   `test_selector_events`, `test_streams`, and `test_sslproto`.
4. An asyncio TLS echo server + client round-trip (loopback,
   self-signed cert, `start_server(ssl=…)`/`open_connection(ssl=…)`)
   passes as a bundled regrtest.
5. `getpeercert()` returns the CPython dict shape for a fixture
   certificate (subject/issuer RDNs, SAN, validity text form,
   serial); an SNI callback fires with the correct arguments and can
   swap contexts.
6. `test_ssl`, `test_socket`, `test_urllib2`, `test_poplib` are
   measured rows (not `skip`), each `pass` or carrying enumerated
   residuals; no hangs under the sweep budget.
7. Package expansion is generic: `test_asyncio` reports per-submodule
   rows in the sweep output and `expectations.toml`.
8. Net label accounting: the sweep's total label count grows by ~30
   (package expansion) and the pass count grows by at least 25 labels
   versus the wave-8 baseline, with no regressions on previously
   green rows.
9. `cargo fmt` / `clippy -D warnings` / `cargo test --workspace` /
   `regrtest --check` all green.

## Drawbacks

- **`_asyncio` is a second implementation of Future/Task semantics.**
  CPython accepts the same cost for the same reason (speed +
  fidelity), and mitigates it exactly the way we will: the test suite
  cross-parametrizes C against Python in-process, so drift is a test
  failure, not a latent bug.
- **A DER/X.509 decoder is new attack-surface-shaped code.** Scope is
  read-only decoding of certificates the TLS layer already validated,
  in safe Rust; the alternative (linking OpenSSL) contradicts the
  RFC 0042 decision to keep TLS memory-safe via rustls.
- **rustls cannot represent all OpenSSL knobs.** The divergence list
  (cipher-string grammar, DH params, ECDH curve selection,
  session_stats) is explicit, documented, and enumerated in test
  expectations rather than silently stubbed — same policy RFC 0042
  set, now with a visible table.
- **Per-submodule expansion grows `expectations.toml` by ~30 rows.**
  Accepted: rows are measurements, and the alternative is one row
  that cannot be green.

## Alternatives

- **Keep asyncio pure-Python and teach the tests to skip C
  parametrizations** (patch the vendored suite): rejected — vendored
  tests are never modified (RFC 0036 invariant), and the C
  implementations carry observable semantics (eager tasks,
  cancellation bookkeeping) that real packages use directly.
- **Implement `_asyncio` in frozen Python under a C-shaped name**:
  rejected — the suite asserts implementation identity
  (`test_asyncio.utils` checks `_CFuture is not _PyFuture` classes
  behave distinctly), and a Python "accelerator" inverts the
  performance story the module exists for.
- **Link real OpenSSL for `_ssl`**: rejected again per RFC 0042 —
  memory-unsafe dependency, platform build matrix, and the ABI story
  WeavePy deliberately avoids. The OpenSSL *shape* is required; the
  OpenSSL *code* is not.
- **Grade `test_asyncio` by curated submodule allowlist** instead of
  generic package expansion: rejected — RFC 0049 retired curated
  allowlists as a scope mechanism; the expansion must be structural
  so new packages get it for free.
- **Proactor/IOCP loop for Windows in this wave**: deferred — the
  Unix selector loop is the ecosystem-critical path; `windows_events`
  keeps its guarded import (RFC 0039) and a Windows loop is its own
  future RFC.

## Prior art

- **CPython** `Modules/_asynciomodule.c` is the specification,
  including its module-state task registry and eager-task fast path
  (gh-97696).
- **uvloop** demonstrates that a from-scratch loop + transports can
  pass `test_asyncio` against the same Python-side package — the
  same target this wave grades against, with the difference that
  WeavePy keeps CPython's own loop code and supplies the layers
  beneath it.
- **rustls in production TLS stacks** (e.g. `rustls` in curl,
  Firefox's NSS-to-rustls experiments) established the
  representational gaps vs OpenSSL are workable behind a
  compatibility surface; WeavePy's divergence list follows that
  precedent.
- **RFC 0042/0039/0040** built the native socket/select/subprocess/
  signal layers this wave composes; **RFC 0053** built the
  source-truth stdlib the verbatim `ssl.py` adoption rides on.

## Unresolved questions

- Whether verbatim `ssl.py` lands wholesale or the adapter delta
  stays >100 lines (rustls handle model vs `_ssl._SSLSocket`
  ownership); acceptance criteria allow either as long as divergences
  are enumerated.
- Whether the `test_socket` IPv6 hang is an engine bug or a sandbox
  property (no IPv6 loopback in some CI sandboxes); if environmental,
  the row records the guard the suite itself uses.
- Whether `test_asyncio/test_windows_events.py` and
  `test_windows_utils.py` rows read `skip` (platform) on the
  Unix sweep — following how the suite itself guards them.
- How much of `test_eager_task_factory` depends on
  `_asyncio.current_task` micro-semantics under eager execution;
  budgeted, but the row may carry a small enumerated residual.

## Results

*To be filled in after implementation, per the RFC 0049 protocol.*
