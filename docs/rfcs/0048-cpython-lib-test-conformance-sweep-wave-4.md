# RFC 0048: CPython `Lib/test/` conformance sweep, wave 4 — verbatim `test.support` + application-stack modules, dict-protocol fidelity, prompt finalization, and main-thread GIL engagement

- **Status**: Accepted
- **Authors**: WeavePy authors
- **Created**: 2026-07-10
- **Tracking issue**: TBD
- **Builds on**: RFC 0038 (wave 3 — binary/codec, filesystem/OS, and CLI
  clusters), RFC 0037 (wave 2 — root-cause clusters + verbatim module
  ports), RFC 0036 (vendored `Lib/test/` checkout + measured
  `expectations.toml` baseline), RFC 0039 (concurrency wave 4 — real OS
  threads with a cooperative GIL), RFC 0026 (multiprocessing), RFC 0024
  (threads/GIL/GC).

## Summary

This is **wave 4 of the conformance sweep**, and it changes the sweep's
character: instead of patching WeavePy's hand-written shims until each
test file passes, the test *infrastructure itself* is now CPython's own
code. The vendored suite runs against a **verbatim CPython 3.13
`test.support` package** (plus its helper modules: `os_helper`,
`import_helper`, `socket_helper`, `script_helper`, `threading_helper`,
`warnings_helper`, `bytecode_helper`, `smtpd`, `asyncore`/`asynchat`,
and friends), and the application-stack modules most tests route through
are verbatim swaps too: the full **`unittest` package (including
`unittest.mock`)**, the **`logging` package** (`config` and `handlers`
included), **`typing`**, **`difflib`**, **`fileinput`**, **`getpass`**,
**`pickletools`**, and **`pkgutil`**.

Running CPython's own helpers is unforgiving: every shortcut the old
shims papered over becomes a live failure. Closing those gaps drove the
rest of the wave — reentrancy-safe dict internals, CPython-timed prompt
finalization, PEP 475 `EINTR` retries, multiprocessing/socket fixes —
and surfaced one genuine architecture bug: **the main thread never held
the GIL**, so it executed bytecode concurrently with worker threads and
let cross-thread GC races corrupt live objects. That is fixed here.

The deliverable is measured: the committed
`tests/regrtest/expectations.toml` baseline is `--check` clean at
**183 `pass`, 30 `fail`, 13 `skip`, 1 `timeout` across 227 rows, with 0
unexpected divergences**, and the full `ci.yml` matrix (fmt, clippy,
workspace tests, MSRV check, blocking regrtest gate) passes locally.

## Motivation

The README's promise is "a 100% compatible, drop-in replacement for
CPython … using CPython's own test suite as a guiding standard." Waves
1–3 made the number auditable and moved it, but they measured against a
*shimmed* `test.support` — so a green row proved "passes under our
approximation of the harness," not "passes under CPython's harness."
Two problems follow:

1. **Shim drift is unbounded.** Every CPython point release grows
   `test.support`; a hand-written shim is a permanent maintenance tax
   and a permanent source of false confidence in both directions
   (shim-only passes *and* shim-only failures).
2. **The harness is the hardest test.** `test.support` exercises the
   interpreter's darkest corners on purpose — reference-count timing,
   GC introspection, signal delivery, subinterpreters, fd inheritance.
   An interpreter that can run it verbatim has proven a large slice of
   the runtime surface for free.

The same argument applies one layer up: `unittest`, `unittest.mock`,
`logging`, and `typing` sit under nearly every test file *and* under
real applications. Where behaviour is defined by CPython, port CPython.

## CPython reference

- **Harness**: `Lib/test/support/` (the whole package, 3.13 branch),
  `Lib/test/libregrtest/`.
- **Application stack**: `Lib/unittest/` (incl. `mock.py`),
  `Lib/logging/` (`__init__`, `config`, `handlers`), `Lib/typing.py`,
  `Lib/difflib.py`, `Lib/fileinput.py`, `Lib/getpass.py`,
  `Lib/pickletools.py`, `Lib/pkgutil.py`, `Lib/subprocess.py`
  (`Popen.__del__` ResourceWarning semantics).
- **Dict protocol**: `Objects/dictobject.c` (reentrant lookup, the
  "dictionary changed size during iteration" RuntimeError, PEP 584
  `|`/`|=`, `fromkeys` as classmethod, view set-algebra), `gh-`issue
  behaviour matched via `Lib/test/test_dict.py` and
  `Lib/test/test_ordered_dict.py`.
- **Finalization timing**: CPython's refcount-driven `tp_dealloc`
  (prompt `__del__`/weakref-callback timing), `Python/ceval.c`
  `POP_EXCEPT` cleanup ordering, `sys.unraisablehook`.
- **Signals / I/O**: PEP 475 ("Retry system calls failing with EINTR"),
  `Modules/socketmodule.c` timeout semantics (notably that
  `SO_RCVTIMEO` does not govern `accept(2)` on macOS — CPython uses its
  own readiness poll), `Lib/test/test_io.py` `CSignalsTest`.
- **Concurrency**: `Python/ceval_gil.c` — the invariant this wave
  restores is CPython's most basic one: *a thread executes bytecode
  only while holding the GIL*, including the main thread.
- **Limits**: `Python/marshal.c` `MAX_MARSHAL_STACK_DEPTH`,
  `Py_C_RECURSION_LIMIT` (used by `functools.lru_cache` re-entry).

## Detailed design

### WS-A — Verbatim `test.support` and application-stack swaps

The eleven flat `test_support_*.py` shim files are deleted and replaced
by a real `test_support/` package containing CPython's sources; the
frozen-module loader in `stdlib/mod.rs` maps it to `test.support.*`.
The same treatment converts `unittest.py`/`unittest_mock.py` into the
full `unittest/` package and `logging.py` into the `logging/` package.
`typing` ships verbatim as `typing_verbatim.py`.

Verbatim code only runs if the interpreter provides what CPython
provides, so the wave closes the native gaps the swaps exposed:

- a pure-Python `_opcode` shim; `_testcapi` extensions
  (`run_in_subinterp` joins non-daemon threads and restores
  `sys` limits, `Py_C_RECURSION_LIMIT`); a `_testinternalcapi` module
  (`get_recursion_depth`, `get_config`, immortalization stubs);
  `_imp._override_frozen_modules_for_tests`.
- `sys.prefix` / `exec_prefix` / `base_prefix` / `base_exec_prefix`;
  `float.__getformat__`; the `time` module's full clock surface
  (`process_time` et al.) with `ctime` routed through
  `libc::localtime_r`.

### WS-B — Dict-protocol fidelity

`Lib/test/test_dict.py` passes in full. The core change is a
reentrancy-safe probe (`dict_reentrant_probe` over `indexmap`'s raw
entry API) that dispatches Python `__hash__`/`__eq__` *without holding
a borrow*, restarting when user code mutates the dict mid-lookup; all
public entry points (`[]`, `in`, `get`, `pop`, `setdefault`, `update`,
`del`) route through it. On top of that: a per-dict structural-version
watch backing CPython's "dictionary changed size during iteration"
RuntimeError; live-cursor dict views (correct `__del__` timing and GC
tracing); PEP 584 `|`/`|=`/`__ror__` including `UserDict` and
`MappingProxyType` dispatch; `dict.fromkeys` as a true classmethod that
constructs subclass instances via `__setitem__`; dict-view set algebra
(`<`, `<=`, `>`, `>=`, `&`-style containment) with exception
propagation; and `__repr__` exceptions surfacing through `repr()`
instead of being swallowed.

### WS-C — Prompt finalization and cycle-GC correctness

CPython frees objects the instant their refcount hits zero; tests
observe that timing. This wave fixes the places WeavePy's prompt-reap
approximation leaked: the compiler now emits `POP_EXCEPT` *after* an
`except` handler's unbind (a `return` out of an `except` block leaked
the live exception — and through `pickle`'s internal `_Stop`, the
entire unpickled object graph); displaced `BoundMethod`s and temporary
class-body functions are reaped; untracked containers holding tracked
children route through the full reap cascade; `PyFile` participates in
cycle traversal/clear so `f.attr = f` cycles die and emit
`ResourceWarning`; buffered/text I/O wrappers gained CPython-faithful
`__del__` finalizers. `marshal` enforces
`MAX_MARSHAL_STACK_DEPTH` instead of overflowing the native stack, and
`lru_cache` enforces a `Py_C_RECURSION_LIMIT` analog.

### WS-D — PEP 475 and socket/multiprocessing hardening

Raw fd reads/writes, `accept`, `sendmsg`, and `recvmsg` retry on
`EINTR` after servicing pending Python signal handlers on the main
thread. `accept` honours `settimeout()` via a `poll()` readiness check
(macOS ignores `SO_RCVTIMEO` for `accept(2)`). Two fd races are closed:
socket teardown now unregisters before `close(2)`, and registry
eviction releases rather than closes a handle whose fd the kernel has
already reused. `ctypes.from_buffer` aliases writable memoryviews
instead of copying (making `multiprocessing.sharedctypes.Value`
actually shared), and the `pickle` accelerator's exception types carry
`__module__ = "pickle"` so they round-trip by qualified name.

### WS-E — Main-thread GIL engagement

The wave's headline correctness fix. Worker threads spawned via
`_thread.start_new_thread` have always acquired the process-wide GIL,
but `Interpreter::run_module_as` — the main program's entry point —
never did. Because both the cooperative hand-off
(`maybe_yield_gil`) and the blocking release (`allow_threads_then`)
no-op on an empty guard stack, the main thread executed bytecode fully
concurrently with whichever worker held the lock. Per-object `GilCell`
mutexes kept individual operations from tearing, but the *collector*
could run on one thread while another mutated the object graph,
misclassifying reachable objects as cyclic garbage. The observable
symptom was `concurrent.futures.ProcessPoolExecutor` intermittently
shipping `_CallItem`s whose `__dict__` had been cleared in the parent
(`AttributeError: '_CallItem' object has no attribute 'fn'`, ~50%
reproduction under fork-context load).

`run_module_as` and `run_site` now push a GIL guard on entry (guarded
against re-entrant calls from the REPL and the in-process conformance
runner) and pop it on exit. Fork children were already correct:
`reinit_after_fork_in_child` rebuilds the lock and restores the
inherited guard depth. The stress reproducer went from ~50% failure to
0/19 failures, with a GIL-holder diagnostic confirming zero collections
run off-GIL.

### WS-F — Harness

`regrtest`'s subprocess runner creates and chdirs into a per-test
temporary directory, so suites that scribble on or delete their cwd
(`test_zipfile`'s `rmtree`) can no longer destroy the build tree.
Expectation rows updated this wave carry measured reasons; suites whose
wall-clock legitimately exceeds the 60s cap under 4-way parallel load
(`test_threading` now that the main thread participates in hand-offs,
`test_pathlib`, `test_json`, `test_multiprocessing_main_handling`)
carry per-row `timeout_seconds`.

## Drawbacks

- **Main-thread GIL cost.** Single-threaded programs pay one
  uncontended acquire at startup — unmeasurable. Threaded programs now
  serialize the main thread with workers, as CPython does; wall-clock
  for thread-heavy suites rises (`test_threading`: ~12s → ~32s
  standalone). This is the price of the invariant, not an accident.
- **Frozen-stdlib size.** Verbatim `test.support`, `unittest`,
  `logging`, `typing`, and `pickletools` add ~24K lines of frozen
  Python. Compile time and binary size grow accordingly.
- **Verbatim modules pin the VM's honesty.** Any VM regression that a
  shim would have masked now fails loudly in unrelated-looking test
  files. That is the point, but it raises the noise floor during
  development.

## Alternatives

- **Keep patching the shims.** Rejected: unbounded drift, and every
  green row remains conditional on the shim's fidelity.
- **Fix the `_CallItem` corruption locally** (pin the objects, defer
  collection around pickling). Rejected: the root cause was a missing
  interpreter-wide invariant; any local patch would leave every other
  cross-thread GC race in place.
- **A stop-the-world barrier inside the collector** instead of engaging
  the GIL on the main thread. Rejected: strictly more machinery to
  reach a weaker guarantee than the one CPython documents and every
  extension assumes.

## Prior art

- **CPython** is the prior art by construction; this wave ports its
  code and its GIL invariant directly.
- **PyPy** runs CPython's test suite with a compatibility-patched
  `lib-python` checkout — the same "run their tests, not a
  reimplementation of their tests" strategy this sweep follows.
- **RustPython** freezes large verbatim slices of the CPython stdlib
  and accepts the binary-size cost, which informed the frozen-package
  loader approach here.
- **GraalPy** documents the same lesson on `test.support`: emulating
  the harness is costlier than implementing the runtime hooks it needs.

## Unresolved questions

- 30 rows remain `fail` (e.g. `test_sqlite3`, `test_tomllib`,
  `test_zoneinfo`, `test_warnings`, `test_typing` residuals) and 1
  `timeout` (`test_weakref`); each carries a measured reason and is
  sequenced for later waves.
- `test_poplib.testTimeoutDefault` showed one connect flake under
  4-way parallel load (passes standalone and on re-run). If it recurs
  in CI, the row needs either a longer per-row timeout or a
  loopback-connect retry in the socket layer.
- The GIL's `holder` bookkeeping is diagnostic-grade, not
  authoritative; promoting it to a checked invariant (debug assertions
  at collection entry) is desirable once the remaining entry points are
  audited.

## Future work

- Sweep the remaining `fail` rows cluster-by-cluster (data formats:
  `sqlite3`/`tomllib`/`zoneinfo`; warnings/typing residuals).
- Faithful `asyncio` on top of the now-correct threading substrate.
- Subinterpreter surface beyond the `_testcapi.run_in_subinterp`
  approximation.
- Revisit the opcode-countdown GIL checkpoint against CPython's
  wall-clock switch interval now that the main thread participates in
  hand-offs (fairness under mixed main/worker load).
