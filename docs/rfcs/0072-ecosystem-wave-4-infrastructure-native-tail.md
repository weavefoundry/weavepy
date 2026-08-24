# RFC 0072: Ecosystem wave 4 — the infrastructure-native tail: gevent over a real greenlet C-API, uvloop, psycopg, and the numpy-selftest re-measure

- **Status**: Accepted
- **Authors**: WeavePy authors
- **Created**: 2026-08-23
- **Tracking issue**: TBD
- **Builds on**: RFC 0066 (ecosystem wave 3: the native greenlet whose
  C-API capsule this wave finishes, and the numpy-selftest census this
  wave re-measures), RFC 0055/0056 (the ecosystem lane and its
  measured-row discipline), RFC 0043–0047 + 0060 (the binary ABI that
  loads cp313 wheels), RFC 0029/0057 (the datetime C-API shell-type
  pattern this wave's `PyGreenlet` shell copies), RFC 0069 (the numpy
  crash burn that retired the census's 12 SIGBUS/SEGV modules), RFC
  0049 (measured-baseline protocol).

## Summary

RFC 0066 grew the ecosystem lane to 37 rows and left exactly one red:
**gevent**, whose Cython modules `PyGreenlet_Import` the
`greenlet._greenlet._C_API` capsule that the WS4 native greenlet never
exported. Its expectations row says so plainly: *"full gevent is the
wave-4 headline row."* This is that wave.

The theme is **infrastructure-native**: the packages that sit under
real deployments rather than inside them — the coroutine substrate
(gevent/greenlet), the event loop (uvloop), the database driver
(psycopg), and the RPC stack (grpcio, stretch). None of them are
importable today or covered by a row, yet they are what separates
"runs Django" from "runs the process Django is deployed in".

The wave lands:

1. **The greenlet C-API capsule, for real.** A byte-faithful
   `PyGreenlet` shell type (`PyObject_HEAD` + `weakreflist` + `dict` +
   `pimpl`, `tp_basicsize == 40` on 64-bit — Cython computes subclass
   field offsets from `sizeof(PyGreenlet)`, so the generic identity-box
   mirror cannot be handed out), bridged to the VM greenlet class the
   same way the RFC 0029/0066 datetime shells bridge to
   `datetime.date`; a 12-slot function-pointer table per upstream
   `greenlet.h` (`PyGreenlet_Type`, `PyExc_GreenletError`,
   `PyExc_GreenletExit`, `New`, `GetCurrent`, `Throw`, `Switch`,
   `SetParent`, `MAIN`, `STARTED`, `ACTIVE`, `GET_PARENT`); an
   importable `greenlet._greenlet` module carrying the `_C_API`
   capsule; and an in-tree gevent-shaped C fixture that
   `PyGreenlet_Import`s, subclasses the type from C exactly as
   Cython's `ctypedef class greenlet.greenlet [object PyGreenlet]`
   does, and switches from inside a C frame — retiring the
   switch-under-C-frame fixture RFC 0066 promised but never landed.
2. **gevent as a measured headline row.** Monkey-patched sockets, a
   spawn/joinall fan-out, cooperative sleep ordering, and a loopback
   echo through the patched socket module, over the real cp313 wheel
   (compiled Cython core, bundled libev/libuv, zope.interface's C
   speedups riding along).
3. **uvloop and psycopg rows.** uvloop (Cython over bundled libuv,
   POSIX-only upstream) probed as a drop-in asyncio loop: policy
   install, task gather, loopback TCP echo, UDP datagrams. psycopg
   (v3 + `psycopg-binary`'s Cython/libpq implementation) probed
   serverless but honestly: `pq.__impl__ == "binary"`, libpq version,
   SQL composition, the adaptation transformer round-trip, conninfo
   parsing, and a connection-refused `OperationalError` through real
   libpq. **grpcio** rides as the stretch row (in-process server +
   unary echo over generic bytes handlers — no protobuf dependency),
   measured whatever its color.
4. **The numpy-selftest re-measure.** The standing `selftest_status =
   "fail"` census is stale twice over: RFC 0069 already burned all 12
   crashing modules to zero-crash, and perf waves 6–8 landed the
   call/attr/collection speedups the census's "collection alone
   measures ~31 min" complaint was waiting on. The lane is re-measured
   post-wave; failures are root-caused and burned or enumerated as
   deselects; the blanket timeout reason is retired. The attrs
   hypothesis skip gets the same re-measure.

As with every wave since RFC 0036, the deliverable is measured: the
manifest grows from 37 to ~40 rows, every touched expectations row is
rewritten from evidence, the full regrtest sweep re-runs at
`unexpected 0`, and the offline wheel lane is verified from a
refreshed cache.

## Motivation

1. **gevent is the only red row, and it was promised.** RFC 0066
   scoped the capsule as stretch and named full gevent "the wave-4
   headline row". The ecosystem lane's honesty depends on stretch
   debts being paid: the greenlet substrate (real stack switching,
   thread-boundness, contextvars) already exists and passes its
   semantics matrix; what is missing is ~one struct, twelve function
   pointers, and a module alias. Leaving the lane at 36/37 over that
   is not a defensible steady state.
2. **The infrastructure layer is what production deployments run.**
   gunicorn's gevent workers, uvicorn's uvloop flag, and psycopg
   under every Django/SQLAlchemy Postgres app are the default
   production topology for Python web services. A drop-in claim that
   covers the framework (Django, FastAPI — RFC 0056/0060) but not the
   worker class, the loop, or the driver is qualified in exactly the
   way adopters notice first.
3. **These are the best remaining engine fuzzers.** Every heavy-native
   row so far has caught real segfault-class bugs (RFC 0057, 0060,
   0069). gevent's corecext and uvloop are the two most
   CPython-internals-adjacent Cython artifacts in common deployment;
   psycopg-binary and grpcio's C++ core bind surface no current row
   touches (libpq async I/O; a C++ completion-queue runtime).
4. **The numpy census is stale and known-stale.** The expectations row
   still claims 12 crash modules (fixed in RFC 0069) and an
   interpreter-speed-bound collection (attacked by waves 6–8).
   A measured baseline that no longer reflects reality is worse than
   a red row — it hides both progress and regressions. Re-measuring is
   cheap; whatever it reveals is the truth the next wave plans from.
5. **Cost of inaction.** The alternative next waves (perf wave 9, the
   embedding story) do not change who can switch interpreters. This
   wave converts the last "cannot run" answer for a major deployment
   shape into a measured surface.

## CPython reference

- **Upstream greenlet 3.2 `greenlet.h`** (not stdlib; the spec for
  WS1): the public `PyGreenlet` struct —

  ```c
  typedef struct _greenlet {
      PyObject_HEAD
      PyObject* weakreflist;
      PyObject* dict;
  } PyGreenlet;
  ```

  — the 12-entry `_PyGreenlet_API` pointer table and its index macros
  (`PyGreenlet_Type_NUM 0` … `PyGreenlet_GET_PARENT_NUM 11`,
  `PyGreenlet_API_pointers 12`), the accessor macros that call through
  the table (`PyGreenlet_MAIN/STARTED/ACTIVE/GET_PARENT` take
  `PyGreenlet*` and return `int`/`PyGreenlet*`), the constructors and
  verbs (`PyGreenlet_New(run, parent)`, `PyGreenlet_GetCurrent()`,
  `PyGreenlet_Throw(g, typ, val, tb)`, `PyGreenlet_Switch(g, args,
  kwargs)`, `PyGreenlet_SetParent(g, nparent)`), and
  `PyGreenlet_Import()` ≡ `PyCapsule_Import("greenlet._greenlet._C_API",
  0)`. The exact header vendored by the gevent sdist is the
  implementation-time source of truth for signatures and slot order.
- **Cython's type import contract**: gevent's `.pxd` declares
  `ctypedef class greenlet.greenlet [object PyGreenlet]`; the
  generated module calls `__Pyx_ImportType("greenlet", "greenlet",
  sizeof(PyGreenlet), check_size)` — it imports the *Python-visible*
  class and checks `tp_basicsize` against `sizeof(PyGreenlet)` (error
  if smaller; warn if larger under the default check). Subclass
  structs (`SwitchOutGreenletWithLoop`, gevent's `Greenlet`) place
  their cdef fields at `sizeof(PyGreenlet)`, which is why the shell's
  basicsize must be exactly 40 and why C-allocated subclass instances
  must keep the base pointer slots inviolate.
- **The RFC 0029/0066 datetime shell pattern**
  (`crates/weavepy-capi/src/datetime_api.rs`): byte-faithful static
  `PyTypeObject` shells with real `tp_basicsize`/`tp_new`/`tp_alloc`,
  lazily bridged to the live VM class so attribute protocol and
  instance crossings work both directions; the
  `try_install_well_known_capsule` lazy installer
  (`capsule.rs`) that this wave extends from `datetime.datetime_CAPI`
  to `greenlet._greenlet._C_API`.
- **Package specs under test**: gevent (cp313 wheel: Cython core
  `gevent._gevent_c*`, bundled libev/libuv loops, zope.interface/
  zope.event deps), uvloop (Cython over bundled libuv; POSIX-only
  upstream — the row carries `status_windows = "skip"` by
  construction), psycopg v3 (`psycopg` pure-Python frontend +
  `psycopg-binary` Cython/libpq implementation; `psycopg.pq.__impl__`
  discriminates), grpcio (C++ core, Cython bindings; generic bytes
  handlers avoid the protobuf dependency in the probe).
- Acceptance harnesses: the bundled `tests/regrtest/` greenlet C-API
  fixture added by this wave, the upstream-shaped
  `tests/capi_ext/_greenletconsumer.c` fixture, the ecosystem probes,
  and numpy's own `numpy._core` suite via the RFC 0066 `installed`
  selftest mode.

## Detailed design

### WS1 — the greenlet C-API: shell type, function table, capsule

**The shell type.** `crates/weavepy-capi/src/greenlet_api.rs` mints a
static `PyGreenlet` shell `PyTypeObject` following `datetime_api.rs`'s
`make_dt_type` pattern:

- Layout: `PyObject_HEAD` + `weakreflist` + `dict` + `pimpl`;
  `tp_basicsize = sizeof(PyGreenlet)` (40 on 64-bit),
  `tp_weaklistoffset = 16`, `tp_dictoffset = 24`,
  `Py_TPFLAGS_DEFAULT | BASETYPE | HAVE_GC`-faithful flag set
  (matching upstream where our GC model allows).
- Bridge: lazily wired to the VM `greenlet` class
  (`greenlet_native::greenlet_class()`, promoted to `pub`) via the
  same `resolve_shell_class` / `faithful_type_for_class` discipline
  datetime uses — Python-level attribute lookups on the shell forward
  to the VM class; VM greenlet instances crossing into C pack a
  faithful 40-byte body registered against the instance; C-allocated
  instances (Cython subclass `tp_alloc`) get a VM `PyInstance` soul
  seeded at alloc time so `green_init`'s `_greenlet_id` registry and
  the whole native switch machinery see them as ordinary greenlets.
- `into_owned(Object::Type(greenlet_class))` and Cython's
  `__Pyx_ImportType` of `greenlet.greenlet` must both resolve to this
  shell pointer (one canonical `PyTypeObject*`), so `tp_base`
  subclassing, `PyObject_TypeCheck`, and the capsule's slot 0 agree.

**The function table.** A `#[repr(C)]` 12-slot table in slot order per
upstream `greenlet.h`, entries `unsafe extern "C" fn` bridging into
`greenlet_native` through a new small `pub(crate)` Rust API (create
from run+parent, switch with args/kwargs, throw, getcurrent, parent
get/set, `main`/`started`/`active` predicates — today all of this is
private `BuiltinFn` plumbing). Error discipline per upstream: verbs
return NULL/-1 with a Python exception set; `PyGreenlet_Switch` on an
unstarted greenlet starts it; `PyGreenlet_Throw` defaults to
`GreenletExit` exactly as the Python-level `throw` does.

**The module and capsule.** `greenlet._greenlet` becomes importable:
the frozen facade binds the native `_greenlet` module as a package
attribute and registers the dotted alias in `sys.modules` (the
`os.path` pattern), so `PyCapsule_Import`'s prefix walk resolves it.
The capsule itself is installed by extending
`try_install_well_known_capsule` for the
`"greenlet._greenlet._C_API"` name (datetime precedent) and eagerly
replacing the facade's `_C_API = None` stub with the real capsule.
New `#[no_mangle]` symbols join `force_link_table.rs`.

**Fixtures.** `tests/capi_ext/_greenletconsumer.c`, compiled against
the stock cp313 headers plus the vendored upstream `greenlet.h`,
doing exactly what gevent's generated C does: `PyGreenlet_Import()`;
type import with the `sizeof(PyGreenlet)` basicsize check; a static
subclass type with a cdef-style field at offset 32, `PyType_Ready`
with `tp_base = PyGreenlet_Type`; instance construction through
`type_call`; `PyGreenlet_New` + `Switch` with value plumbing; `Throw`
with default `GreenletExit`; the four accessor macros; and a
**switch-from-inside-a-C-frame** leg (Python → C → switch → back)
that retires RFC 0066's missing fixture. A bundled regrtest
(`tests/regrtest/test_greenlet_capi.py`) drives it.

### WS2 — gevent as a measured headline row

The existing row and probe stand; the work is measurement and the
burn. Sequence: with WS1 landed, re-run the row; root-cause the next
first-failure; burn or enumerate. Known-suspect surfaces going in,
from reading what the wheel binds: the corecext event loop's
`PyObject_CallFunctionObjArgs`-heavy callback dispatch, zope.interface
`_zope_interface_coptimizations` (a C metaclass user), gevent's
`monkey.patch_all` rebinding `_socket`/`_ssl`/`threading` internals
(the RFC 0054 `_ssl` and RFC 0039 threading surfaces), and libev
fd-watcher semantics over the VM's socket fds. The probe grows a
gevent-native leg beyond the current monkey-patch script: a
`gevent.Greenlet` subclass (the compiled-Cython-subclass shape),
`gevent.queue` producer/consumer, and `gevent.Timeout`.

Expectations: `status = "pass"` is the target; if a residual survives
the wave it is enumerated with a root-caused reason — but a red
gevent row no longer names the capsule.

### WS3 — uvloop

Manifest row (POSIX-only upstream; `status_windows = "skip"` with
that reason). Probe: `uvloop.new_event_loop()` +
`asyncio.set_event_loop_policy(uvloop.EventLoopPolicy())`; a task
fan-out with `asyncio.gather`; a loopback TCP echo
(`loop.create_server` / `open_connection`) over the uvloop transports;
a UDP datagram endpoint round-trip; `loop.getaddrinfo("localhost")`;
`run_in_executor` handoff; clean loop close with no pending-handle
warnings. Known-suspect surfaces: uvloop's Cython transports mint
memoryviews over receive buffers (the RFC 0066 WS1 buffer surface),
its signal handling wants `signal.set_wakeup_fd` fidelity, and its
`loop.subprocess_exec` leg binds the RFC 0040 process surface — the
probe includes a `subprocess_exec` echo leg, deselectable with a
reason if it proves a rabbit hole.

### WS4 — psycopg (+ grpcio stretch)

**psycopg**: requirements `psycopg[binary]` (the v3 frontend + the
Cython/libpq binary implementation). Probe (serverless but
behavior-asserting): assert `psycopg.pq.__impl__ == "binary"` (proves
the C extension is live, not the pure-Python fallback);
`pq.version()` sanity; `sql.SQL(...).format(sql.Identifier/Literal)`
composition to string; the adaptation layer round-trip
(`Transformer.dump`/`load` for int/float/str/bytes/list); conninfo
`make_conninfo`/`conninfo_to_dict`; and a real libpq exercise: connect
to a closed loopback port with `connect_timeout=1` and assert
`OperationalError` (not a crash, not a hang). Windows: binary wheels
exist; measured advisory per standing policy.

**grpcio (stretch)**: probe builds an in-process server on a loopback
port with a `GenericRpcHandler` (bytes-identity serializers — no
protobuf), a unary-unary echo, and a client channel round-trip with a
deadline. Measured whatever its color; a red row with a precise
reason is the wave-5 worklist (grpcio's C++ core is the largest
binary artifact the lane would load).

### WS5 — the numpy-selftest re-measure

- Re-run the `installed`-mode `--pyargs numpy._core` lane as-is and
  record fresh numbers (collection time first — the stale census
  measured ~31 min pre-waves-6–8).
- If collection still busts the 2400 s budget: split the lane by
  module (the manifest `command` takes any pytest args; per-module
  `--pyargs numpy._core.tests.test_multiarray`-style lanes under one
  row via a driver, or an honest budget raise if it is close).
- Burn the top failure clusters from the fresh census; every
  remaining failure becomes an enumerated `deselect` with a
  root-caused reason or a named residual class in the row reason.
- The acceptance bar: the blanket "collection times out" reason is
  retired; `selftest_status` reaches a **fresh measured verdict** —
  `pass` with enumerated deselects is the target; a `fail` with a
  post-wave census (counts, clusters, reasons) is the documented
  fallback.
- attrs gets the same treatment: re-run the 2400 s suite once on the
  wave-8 interpreter; flip the skip to a measured verdict if it now
  fits, else refresh the skip reason with the new measured runtime.

### WS6 — re-measure and re-baseline

Per the RFC 0049 protocol: the full regrtest sweep re-runs
(`--mode subprocess --jobs 8`) at `unexpected 0`; every touched
ecosystem row is rewritten from evidence; `ecosystem_fetch.py` learns
the new rows so the offline `--wheels` lane covers them; CI's cache
key picks up the manifest change automatically. New bundled
regrtests: `test_greenlet_capi.py` (WS1) and any engine-burn
regressions per standing policy (every engine fix lands a fixture).

### Acceptance criteria

1. The `_greenletconsumer.c` fixture compiles and passes on macOS and
   Linux CI: capsule import, basicsize-checked type import, C
   subclassing with a field at offset 32, `New`/`Switch`/`Throw`/
   `GetCurrent`/`SetParent` and the four accessor macros, and the
   switch-under-C-frame leg. Windows measured; advisory per RFC
   0063/0064.
2. The **gevent row passes** (probe: monkey-patch, spawn/joinall,
   cooperative sleep, patched-socket echo, plus the new
   Greenlet-subclass/queue/Timeout legs) on macOS and Linux, offline
   from the wheel cache.
3. **uvloop and psycopg rows pass** on macOS and Linux with the probes
   as specified (uvloop `status_windows = "skip"`, upstream-truthful).
   grpcio lands measured whatever its color, reason mandatory if red.
4. The numpy selftest lane reaches a fresh measured verdict per WS5;
   the stale census (12-crash claim, 31-min collection claim) is
   retired from `expectations.toml`. The attrs selftest skip is
   re-measured (flipped or re-reasoned with fresh numbers).
5. The ecosystem manifest carries ≥ 40 measured rows; no row's reason
   references work that has already shipped.
6. No regrtest regressions: `unexpected 0` on the final sweep; the
   greenlet semantics matrix and the new C-API fixture pass under the
   default (JIT-on) build.
7. `cargo fmt` / `clippy -D warnings` / `cargo test --workspace` /
   `regrtest --check` / `ecosystem --check` all green.

## Drawbacks

- **The C-API now hands out C-writable greenlet instances.** A Cython
  subclass allocates the base struct and expects `weakreflist`/`dict`
  slots it can see; WeavePy must keep the shell body and the VM
  `PyInstance` soul coherent for the object's whole life. Mitigation:
  this is the datetime-shell discipline already proven under pandas'
  `_NaT`/`Timestamp` subclasses; the fixture's offset-32 field leg
  exists to catch layout drift.
- **gevent's monkey-patching rebinds stdlib internals under test.**
  A green gevent row means `socket`/`ssl`/`threading` survive being
  rebound mid-process — failures here can implicate long-stable
  surfaces. Mitigation: the probe runs in a subprocess venv (harness
  default), so patching never leaks into the harness.
- **uvloop and grpcio are large opaque binaries.** When they fail,
  root-causing means reading generated Cython/C++ against our ABI.
  Mitigation: the stretch/measured-row discipline — a red row with a
  precise first-failure reason is an acceptable landing for grpcio
  (not for gevent/uvloop/psycopg, which are this wave's acceptance).
- **The numpy re-measure may still be budget-bound.** If collection
  remains over budget even post-waves-6–8, the lane-split machinery
  adds harness complexity for one row. Mitigation: WS5 names the
  split as in-scope work, and the budget raise is the documented
  cheaper fallback if the overshoot is marginal.
- **Wheel-cache growth**: gevent + uvloop + psycopg-binary + grpcio
  add ~60 MB per platform. Same posture as RFC 0056/0066: exact pins
  live in the manifest, CI cache keys on the manifest hash.

## Alternatives

- **Load the real greenlet cp313 wheel now that we have a C API**:
  still rejected, same grounds as RFC 0066 — upstream's C core
  rewrites CPython `PyThreadState` internals and slices the C stack
  around CPython's frame layout; the capsule makes our *native*
  greenlet speak upstream's ABI instead.
- **Publish the VM greenlet class through the generic
  `install_user_type` mirror instead of a faithful shell**: rejected —
  the generic identity box's basicsize exceeds `sizeof(PyGreenlet)`,
  and Cython subclasses place fields at offset 32; anything but an
  exact 32-byte base layout corrupts subclass instances. This is the
  precise lesson the datetime shells already encode.
- **Patch gevent to use pure-Python greenlet paths** (GraalPy-style
  package overrides): rejected as primary — the point of the lane is
  unmodified wheels; overrides hide ABI gaps the next package trips
  over. Kept in reserve only if a single unfixable residual gates an
  otherwise-green row, and then as an enumerated, documented patch.
- **psycopg2 instead of psycopg v3**: v3 chosen — it is the actively
  developed line, its binary implementation is Cython (our
  best-covered generator), and its pure-Python fallback gives a
  built-in control (`pq.__impl__` proves which one we exercised).
  psycopg2 remains future work.
- **Skip uvloop because asyncio already passes**: rejected — uvloop
  replaces the loop *implementation* underneath the same API; it
  binds transports, ssl handshakes, and subprocess plumbing at the C
  level that `test_asyncio` never touches. That is exactly the
  fuzzing value.

## Prior art

- **PyPy** ships its own greenlet and exposes the upstream C API over
  it for Cython consumers (`greenlet._greenlet` as a cpyext module) —
  the same shape WS1 builds; its compatibility notes flag the same
  subclass-layout hazard.
- **The RFC 0029 → 0057 datetime capsule lineage**: shell types with
  exact C layout lazily bridged to VM classes, a well-known capsule
  installed on first import, and a header-proof fixture — WS1 is that
  pattern applied to a third-party ABI for the first time.
- **RFC 0055/0056/0066 measured-row discipline**: reds allowed,
  reasons mandatory, stretch debts named — this wave pays RFC 0066's
  two named debts (the capsule, the census).
- **gunicorn/uvicorn deployment docs** treat gevent workers and
  uvloop as the default production performance knobs — the external
  evidence for calling this layer "infrastructure".

## Unresolved questions

- **Whether gevent's corecext loop imports cleanly or falls back to
  its cffi loop.** The wheel carries both; cffi is not a proven
  surface in WeavePy. If corecext hits an unfixable residual, the
  honest outcomes are (in order): burn it, enumerate it and pass on
  the cffi loop if cffi happens to work, or land the row red with the
  reason. Measured in week one.
- **How much of `monkey.patch_all` holds.** Patching `threading`
  internals depends on implementation details (`_active`,
  `_DummyThread`) that WeavePy models but has never had rebound
  mid-process. The probe measures it; partial-patch (`patch_all(
  thread=False)`) is the documented fallback with a reason.
- **uvloop's reliance on `asyncio` C internals.** uvloop imports
  private `asyncio` pieces per version; if it binds `_asyncio`
  accelerator internals we do not export, the burn may extend RFC
  0054's `_asyncio` surface. In-scope if bounded; enumerated residual
  if not.
- **The numpy fresh census's shape.** If the failure clusters are
  dominated by one engine gap (e.g. a dtype-promotion or scalar-math
  family), burning it may be a bigger prize than this wave budgeted;
  the RFC allows carrying a named cluster to wave 5 rather than
  rushing it.
- **Windows measurement for gevent/psycopg.** Windows lanes remain
  advisory (RFC 0063/0064); rows get measured Windows statuses where
  CI reveals them, but stamping `measured_os` for Windows stays out
  of scope.

## Future work

- **grpcio to a green row**, and the psycopg2 line, once this wave's
  reds are enumerated.
- **scipy/Pillow/lxml upstream selftests** via the `installed` mode
  (RFC 0066 named them; the numpy lane's re-measure mechanics apply
  directly).
- **scikit-learn** (scipy + joblib/loky process pools) as the next
  matrix rung.
- **A gevent-workers capstone**: gunicorn with `-k gevent` serving a
  Django app end-to-end — the deployment-shaped successor to RFC
  0056's Django capstone.
- **The fiber/PEP 703 design note** RFC 0066 promised: the greenlet
  C API freezes more of the switching substrate's contract; the
  free-threading RFC inherits it.
