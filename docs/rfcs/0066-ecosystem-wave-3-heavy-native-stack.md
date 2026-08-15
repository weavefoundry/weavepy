# RFC 0066: Ecosystem wave 3 — the heavy-native stack: scipy, Pillow, lxml, a native greenlet, and the numpy-selftest capstone

- **Status**: Draft
- **Authors**: WeavePy authors
- **Created**: 2026-08-13
- **Tracking issue**: TBD
- **Builds on**: RFC 0055/0056 (the ecosystem lane and its measured-row
  discipline), RFC 0043–0047 + 0060 (the binary ABI that loads cp313
  wheels, incl. the numpy/pandas capstones), RFC 0028/0057 (buffer
  protocol + memoryview surface), RFC 0029/0062 (datetime C-API and its
  known shell-type deferral), RFC 0024/0025 (GIL + cross-thread heap,
  which the greenlet design must respect), RFC 0049 (measured-baseline
  protocol).

## Summary

RFC 0056 proved the modern web/data stack (Django, pydantic, numpy
wheels); RFC 0060 added the pandas and FastAPI capstones. The ecosystem
lane now stands at 31/31 — and every remaining name on its deferred
list is *heavy-native*: **scipy** (Cython + Fortran + vendored
OpenBLAS), **Pillow** (a large hand-written C extension), **lxml**
(the biggest Cython artifact on PyPI, bundling libxml2/libxslt), and
**greenlet** (hand-written per-platform stack switching that no
frame-model emulation fully replaces). These four are the packages
RFC 0062's future-work section named as the next matrix expansion, and
they are what separates "runs Django" from "runs the scientific and
async-io ecosystems".

This wave lands them as measured rows, plus the engine work they force:

1. **The multi-dimensional buffer endgame.** `test_buffer`'s
   expectations row is explicit: the residuals need CPython's
   `_testbuffer` C module, and "a stub would regress pickletester" —
   only a real multi-dim `ndarray` exporter closes it. We compile
   CPython's `Modules/_testbuffer.c` verbatim as a `tests/capi_ext`
   fixture and finish the indirect-buffer (`PyBUF_INDIRECT` /
   `suboffsets`) legs of the VM memoryview and the C-API buffer core.
   This is also exactly the surface Pillow and scipy's Cython
   memoryviews lean on.
2. **The datetime shell-type bridge.** RFC 0062's header-proof fixture
   discovered that `PyObject_CallMethod((PyObject *)
   PyDateTimeAPI->DateType, "today", NULL)` hits the C-side shell type
   instead of the Python-visible `datetime.date`. The shells learn to
   answer the attribute protocol through their bridged VM class — a
   correctness fix any date-handling extension (pandas, lxml's
   schema types, Pillow's EXIF helpers) can trip over.
3. **A native `greenlet`.** The real cp313 greenlet wheel manipulates
   CPython `PyThreadState` internals and slices the C stack; it cannot
   work against WeavePy's runtime. Following PyPy's precedent (PyPy
   ships its own greenlet over `_continuation`), WeavePy implements
   greenlet natively — but *unlike* PyPy's frame-model start, WeavePy's
   evaluator recurses on the native Rust stack (`run_frame` →
   `call_python_owned` → `run_frame`, with C-extension frames
   interleaved via `PyObject_Call` re-entry), so the design is **real
   stack switching**: each started greenlet runs on its own native
   stack, and a switch swaps the native stack plus the per-interpreter
   spine (`frame_stack`, `exc_info_stack`, recursion depth,
   contextvars). That is the only design that survives
   `Python → C → Python → switch()`, which is the shape gevent needs.
4. **The numpy-selftest capstone.** The numpy row today is an unpinned
   wheel and a smoke probe. The wave pins it, teaches the selftest
   runner an *installed-package* mode (`pytest --pyargs` from a neutral
   cwd — numpy's sdist cannot be tested unbuilt), and runs a named
   subset of numpy's own suite as the heavy-native capstone RFC 0062
   called for.

As with every wave since RFC 0036, the deliverable is measured: the
ecosystem manifest grows from 31 rows to ~37 (scipy, pillow, lxml,
greenlet non-stretch; matplotlib and gevent stretch), every row graded
against a checked-in baseline (reds allowed, reasons mandatory), the
full regrtest sweep re-runs, and every touched expectations row is
rewritten from evidence.

## Motivation

1. **The deferred list is the adoption blocker.** The current 31 rows
   cover web and data-model workloads; a person whose project imports
   scipy, PIL, or lxml — i.e. most scientific, imaging, and scraping
   codebases — still cannot switch. RFC 0062 already named this exact
   set ("the scipy/Pillow/lxml/greenlet matrix expansion" and "numpy's
   own suite is the obvious capstone") as future work; this wave is
   that work.
2. **Heavy-native rows are the best engine fuzzer we have.** Every
   ecosystem capstone so far has caught real segfault-class engine
   bugs (RFC 0057: the `datetime_CAPI` shadow capsule; RFC 0060: the
   `PyType_FromMetaclass` NULL-`tp_alloc` crash, the
   `CALL_FUNCTION_EX` kwargs-clone leak). scipy and lxml are the two
   largest Cython artifacts on PyPI; Pillow is a decades-old
   hand-written C extension. Whatever ABI dark corners remain, these
   three will find them.
3. **`test_buffer` is the last *architectural* red row.** The other
   25 fail rows are residual burns or codegen-stage emulation; the
   buffer row is the only one whose reason says "left for a dedicated
   workstream" because it needs new engine capability (indirect
   buffers + a real multi-dim exporter). This wave is that workstream,
   and the same capability is load-bearing for Pillow
   (`Image.frombuffer`, numpy interop) and scipy (Cython typed
   memoryviews over strided ndarrays).
4. **greenlet gates an entire ecosystem.** gevent, its monkey-patched
   world, and everything `gevent`-adjacent (locust, some celery
   deployments) sit on greenlet; SQLAlchemy's asyncio bridge imports
   greenlet unconditionally (`sqlalchemy.util.greenlet_spawn` powers
   `AsyncSession`). Today `import greenlet` is an ImportError and the
   cp313 wheel can never work. A native greenlet turns a hard wall
   into a measured surface — and its stack-switching substrate is the
   same machinery a future PEP 703 / fiber story would want.
5. **Cost of inaction.** The conformance tail elsewhere (codegen-stage
   emulation, `_testcapi` legs) makes the baseline number better but
   does not change who can switch interpreters. Leaving the
   heavy-native set red keeps "drop-in replacement" qualified to
   pure-Python-plus-pandas workloads.

## CPython reference

- **PEP 3118**, `Modules/_testbuffer.c` (~2,800 lines) — the
  `ndarray` test exporter: the full `PyBUF_*` request matrix,
  multi-dimensional shapes/strides, **suboffsets** (PIL-style indirect
  arrays), `getbuf`/`memoryview` round-trips, `staticarray`, and the
  `ND_*`/`PyBUF_*` module constants `test_buffer` imports. Compiled
  verbatim, it is both the acceptance harness and the spec for the
  indirect-buffer legs.
- `Objects/memoryobject.c` — indirect-access item paths
  (`lookup_dimension` chasing `suboffsets`), `tolist` over indirect
  views, contiguity classification, `PyBuffer_GetPointer`,
  `PyBuffer_ToContiguous` / `PyBuffer_FromContiguous`,
  `PyMemoryView_FromBuffer`.
- `Include/datetime.h` + `Modules/_datetimemodule.c` — the
  `PyDateTime_CAPI` struct: the six type objects are *the same
  objects* as the Python-visible classes in CPython, so
  `PyObject_CallMethod` on `DateType` just works there; WeavePy's
  byte-faithful shells (RFC 0029) must forward the attribute protocol
  to match.
- **greenlet** (not stdlib; upstream `greenlet/greenlet.c` +
  `greenlet.h` are the spec): `greenlet(run, parent)`, `switch(*args,
  **kwargs)` value plumbing (tuple/dict/single-value rules), `throw()`
  defaulting to `GreenletExit`, parent-chain propagation of both
  return values and exceptions, `GreenletExit` *not* propagating (it
  becomes the switch value in the parent), settable `parent` with
  cycle rejection, thread-boundness (`error: cannot switch to a
  different thread`), `getcurrent()`, `gr_frame`, `gr_context`
  (contextvars, greenlet ≥ 1.0 semantics), `__bool__` (started and
  not dead), GC of unstarted and suspended greenlets (throwing
  `GreenletExit` into collected suspended greenlets), and the C-API
  capsule `greenlet._C_API` (`PyGreenlet_New/Switch/Throw/GetCurrent`,
  the `PyGreenlet_Import` header dance) that gevent's Cython modules
  bind.
- Package specs under test: scipy (cp313 wheels bundle OpenBLAS +
  libgfortran via auditwheel/delocate — the vendored-dylib loading
  path the numpy wheel row already proves), Pillow (`PIL._imaging`),
  lxml (`lxml.etree` bundling libxml2/libxslt), numpy's own
  `numpy/_core/tests` suite (pytest + hypothesis).
- Acceptance tests: `Lib/test/test_buffer.py` (the flip),
  `test_memoryview.py` (must not regress), the bundled
  `tests/regrtest/` greenlet/buffer/datetime fixtures added by this
  wave, and the upstream greenlet test suite as a selftest row.

## Detailed design

### WS1 — the multi-dimensional buffer endgame + `_testbuffer`

**Fixture**: vendor CPython's `Modules/_testbuffer.c` into
`tests/capi_ext/` (same discipline as the `_testcapi` legs: verbatim
where possible, `#ifdef`-gated only where it touches CPython
internals we do not model). `weavepy-capi/build.rs` compiles it; the
regrtest harness makes it importable exactly like `_numpylike`.

**Engine legs it forces** (measured against the fixture, then against
`test_buffer`'s ~70-test `TestBufferProtocol` class):

- `weavepy-capi/src/buffer.rs`: real `PyBUF_INDIRECT` support — the
  internal view box carries `suboffsets`; native exporters keep
  `suboffsets = NULL` (correct — no WeavePy builtin is indirect), but
  the *consumer* paths (`PyBuffer_GetPointer`, item lookup,
  `PyBuffer_ToContiguous`/`FromContiguous`, contiguity classification)
  chase suboffsets per `memoryobject.c`'s `lookup_dimension`.
- `PyMemoryView_FromBuffer` over foreign indirect views, and
  memoryview operations on them: `m[i, j]` get/set, `tolist`,
  equality (element-wise across different layouts), `cast` legality
  checks, `hash` only for contiguous read-only views, and the exact
  error taxonomy (`NotImplementedError` for multi-dim sub-views and
  slicing, matching what the VM already does for the direct case).
- `struct`-format item unpacking for the full format set
  `_testbuffer` exercises (the native `struct` module already exists;
  this is wiring memoryview's item codec through the same tables).

`test_buffer` flips from `fail` to a measured verdict; the four named
`PyBUF_*`-constant errors disappear because the constants now come
from the real fixture. `test_memoryview` and
`test_picklebuffer`/`pickletester` are re-measured to prove the
"a stub would regress" hazard stayed theoretical.

### WS2 — datetime shell types answer the attribute protocol

`crates/weavepy-capi/src/datetime_api.rs` mints six byte-faithful
`PyTypeObject` shells with a lazily-bound `bridge` to the live VM
class. The fix: the shells' `tp_getattro` forwards unknown attribute
lookups to the bridged VM class (returning bound classmethods like
`date.today` as callables that route through the RFC 0022 call
bridge), while keeping the C-slot surface (`tp_new`, `tp_basicsize`,
the `PyDateTime_GET_*` macro layout) exactly as RFC 0029 shipped it.
`PyObject_CallMethod((PyObject *)PyDateTimeAPI->DateType, "today",
NULL)` then returns a real `datetime.date`.

The RFC 0062 header-proof fixture grows the exact call shapes that
were discovered broken; the `markupsafe_sdist` selftest deselect
(str-subclass escape through the shell-type C-ABI, the same residue
class) is re-measured and the deselect retired if the same bridge
closes it.

### WS3 — scipy, Pillow, lxml rows + the C-API tail they surface

Three non-stretch manifest rows, each a behavior-asserting probe over
the real PyPI binary wheel (the vendored-shared-library loading path
— delocate `.dylibs` / auditwheel `.libs` — is already proven by the
numpy wheel row):

| Row | Requirements | Probe asserts |
|---|---|---|
| `scipy` | `scipy` (+ numpy) | `linalg.solve` + `lu_factor` round-trip against numpy reference; `sparse.csr_matrix` matvec + format conversions; `optimize.minimize` (BFGS) converges on Rosenbrock; `integrate.quad` value+tolerance; `stats.norm` pdf/cdf/rvs shapes; `fft.fft` ↔ `ifft` round-trip; a Cython typed-memoryview path (`scipy.ndimage.uniform_filter`) over a strided ndarray |
| `pillow` | `pillow` | `Image.new` + `ImageDraw` primitives; resize/rotate/crop; PNG and JPEG save→load round-trips through `io.BytesIO` (pixel equality for PNG); `tobytes`/`frombytes`; `Image.fromarray`/`np.asarray(img)` numpy interop (the WS1 buffer surface); EXIF read on a synthesized JPEG |
| `lxml` | `lxml` | `etree.fromstring`/`tostring` round-trip; XPath with namespaces; XSLT transform; `iterparse` over a bytes stream; `lxml.html` fragment parse + link rewrite; validation via `XMLSchema`; interop: `etree` element passed through `copy.deepcopy` and pickled `tostring` |

Engine work here is deliberately **measured-first**: the probes run,
first failures are root-caused, and the C-API tail is burned exactly
as RFC 0055 WS-style waves did (the mypyc tail, the PyO3 audit).
Known-suspect surfaces going in, from reading what these wheels bind:
`PyCapsule` destructor ordering (scipy's `LowLevelCallable`),
`PyUnicode_AsUTF8AndSize`-family width/kind internals (lxml is the
heaviest `PyUnicode` C consumer on PyPI), Pillow's
`PyErr_SetFromErrnoWithFilenameObject` + `getbuffer` write paths, and
Cython's exception-swap (`PyErr_Fetch`/`SetExcInfo`) depth under
nested generators. Each engine fix lands a bundled regrtest per
standing policy; anything that survives the wave becomes an
enumerated residual on the row.

### WS4 — a native greenlet over real stack switching

**Why not frames-only.** WeavePy's evaluator is a recursive
tree-walker: every Python activation is a native Rust frame
(`run_until_yield_or_return` under `stacker::maybe_grow`), and C
extensions re-enter the eval loop recursively through the
thread-local `ActiveContext` (`Python → C → Python` interleaves
native frames). A frame-model greenlet (parking heap `Frame`s the way
generators do) can only switch when no C frame sits between the
current activation and the switch target — which excludes the
`sqlalchemy.greenlet_spawn` and gevent shapes that are the entire
point. So the substrate is **native stack switching**.

**Substrate.** A new `weavepy-vm/src/greenlet_native/` module family:

- Each started greenlet owns a dedicated native stack (default 1 MiB,
  guard page below, `mmap`-allocated; size configurable via
  `WEAVEPY_GREENLET_STACK_SIZE`). Switching is a small, well-audited
  `unsafe` core — callee-saved registers + stack pointer swap per
  platform (aarch64 + x86_64 for macOS/Linux/Windows), the same
  contract as upstream greenlet's platform switch files, implemented
  either directly or over the `corosensei` crate if its stack
  ownership model fits our GIL story (decided at implementation time;
  the RFC commits to the *semantics*, not the crate).
- **`stacker` interaction**: greenlet stacks disable segmented-stack
  growth (`maybe_grow` becomes a plain call under a greenlet;
  recursion depth still guards via `recursion::DEPTH`). Switching
  away mid-segment on the *main* stack is safe (segments stay alive
  until their frames return), but not growing new segments on
  greenlet stacks keeps the unwinding story simple.
- **What a switch swaps** (per the thread's `Interpreter`):
  `frame_stack` (the `FrameShell` spine that `sys._getframe`/tracing
  read), `exc_info_stack`, `recursion::DEPTH`, and the contextvars
  current-context (greenlet ≥ 1.0 `gr_context` semantics). The GIL is
  *held across the switch* — greenlets are same-thread by definition,
  so no lock traffic; the C-API `ActiveContext` is thread-local and
  therefore already correct.

**Python surface.** A native `_greenlet` module + frozen
`greenlet/__init__.py` facade (version-stringed to the upstream line
it models): `greenlet(run=None, parent=None)`, `switch` /
`throw` with upstream's exact value-plumbing rules, `getcurrent()`,
per-thread main greenlet, settable `parent` with cycle rejection,
thread-boundness errors, `dead`/`__bool__`, `gr_frame` (the parked
spine's top shell), `gr_context`, `GreenletExit` (inherits
`BaseException`, does not propagate — becomes the parent's switch
value), unhandled-exception propagation to the parent chain, and GC
behavior: collecting a *suspended* greenlet throws `GreenletExit`
into it on its own stack (upstream semantics; the finalizer runs
through the RFC 0024 GC hooks).

**C-API capsule (stretch, for gevent).** `greenlet._C_API` exporting
the `PyGreenlet_*` table per upstream `greenlet.h`, so Cython modules
compiled against greenlet headers (gevent's `_gevent_c*`) can
`PyGreenlet_Import`. The gevent row (WS6) measures how far this
carries; a red gevent row with a precise reason is an acceptable
wave outcome, a red greenlet row is not.

**Distribution.** greenlet joins the bundled third-party set the way
numpy's facade did (RFC 0030), with the RFC 0055 site-packages
precedence caveat inverted: a pip-installed cp313 greenlet wheel
*cannot* work here, so `_minipip`/pip resolution treats `greenlet` as
satisfied by the bundled distribution (a real
`greenlet-<version>.dist-info` in the stdlib path so
`importlib.metadata`, pip's resolver, and dependents like SQLAlchemy
see an installed distribution instead of downloading the wheel). The
ecosystem row installs nothing and probes the bundled module;
SQLAlchemy's existing row grows an `AsyncSession` probe leg
(`greenlet_spawn` is exactly the Python→C→Python switch shape).

### WS5 — the numpy-selftest capstone (+ harness `installed` mode)

- The selftest runner (`crates/weavepy-conformance/src/ecosystem.rs`)
  learns `mode = "installed"`: skip sdist extraction, run
  `python -m pytest <command>` from a neutral temp cwd against the
  *installed* package (`--pyargs`). Required because numpy's sdist is
  untestable unbuilt, and the sdist tree would shadow the wheel.
- The numpy row is pinned (`numpy==<current 2.x, measured at
  implementation time>`) and gains:

```toml
[packages.numpy.selftest]
mode = "installed"
requirements = "pytest hypothesis"
command = "--pyargs numpy._core -q"
timeout_seconds = 2400
```

- Scope honesty: `numpy._core` is the C-API/ufunc/dtype heart
  (~the majority of numpy's suite by value); full-suite
  (`--pyargs numpy`) is attempted and recorded in `notes`, but the
  acceptance bar is the `_core` lane, because the attrs precedent
  shows hypothesis-heavy suites can be interpreter-speed-bound —
  a `selftest_status = "skip"`-with-measured-reason outcome on the
  *full* suite is acceptable; on the `_core` lane it is not without
  enumerated deselects.

### WS6 — stretch rows: matplotlib and gevent

Landed as measured rows whatever their color (the RFC 0056 stretch
discipline — a red row with a precise reason is the wave-4 worklist):

- `matplotlib`: Agg backend only (`MPLBACKEND=Agg`), probe renders a
  line+scatter figure with labels to PNG via `FigureCanvasAgg`,
  asserts dimensions + non-blank pixel statistics, round-trips
  through Pillow. Exercises `ft2font`, `kiwisolver`, and the WS1/WS3
  surfaces together.
- `gevent`: probe monkey-patches, runs a `gevent.spawn` fan-out with
  `joinall`, a `gevent.sleep` ordering assertion, and a loopback
  socket echo through the patched socket module. Rides the WS4 C-API
  capsule; measured honestly.

### WS7 — re-measure and re-baseline

Per the RFC 0049 protocol: two full sweeps
(`regrtest --all-cpython --mode subprocess --jobs 8`) cross-checked;
every touched row rewritten from evidence (`test_buffer`,
`test_memoryview`, `test_picklebuffer`, and any rows the C-API tail
moves); the ecosystem baseline committed fully measured with the
offline lane verified from a refreshed wheel cache
(`ecosystem_fetch.py` learns the new pins, including the platform
wheels for scipy/pillow/lxml/matplotlib/gevent). New bundled
regrtests: the greenlet semantics matrix (switch value plumbing,
parent-chain propagation, `GreenletExit` swallowing, thread-boundness,
suspended-greenlet GC, contextvars isolation, and a
**switch-under-C-frame** fixture that routes a switch through a
C-extension callback), the indirect-buffer matrix (suboffsets get/set/
tolist/compare via `_testbuffer.ndarray`), and the datetime
shell-bridge call shapes.

### Acceptance criteria

1. `_testbuffer` compiles and imports as an in-tree fixture on the
   three CI platforms; `test_buffer` reaches a measured verdict past
   the four `PyBUF_*` errors with residuals below 5, enumerated;
   `test_memoryview` and `test_picklebuffer` do not regress.
2. The datetime shell types answer the attribute protocol through the
   bridged VM classes; the header-proof fixture's new call shapes
   pass; the `markupsafe_sdist` deselect is re-measured (retired or
   re-reasoned from evidence).
3. `scipy`, `pillow`, and `lxml` rows **pass** with
   behavior-asserting probes, offline from the wheel cache.
4. The native greenlet passes the bundled semantics matrix including
   the switch-under-C-frame fixture; the `greenlet` row passes; the
   upstream greenlet test suite runs as a selftest with a measured
   verdict (deselects allowed with reasons — C-implementation-detail
   legs expected); SQLAlchemy's row gains a green `AsyncSession` leg.
5. The numpy row is pinned and its `installed`-mode selftest lane on
   `numpy._core` reaches a measured verdict — pass, or enumerated
   deselects each with a root-caused reason; the full-suite attempt
   is recorded in `notes`.
6. The ecosystem manifest carries ≥ 36 measured rows; all four
   non-stretch new rows (scipy, pillow, lxml, greenlet) pass;
   matplotlib and gevent land measured whatever their color.
7. At least 1 net regrtest label flips red→green (`test_buffer`), no
   regressions, `unexpected 0` on the final sweep.
8. `cargo fmt` / `clippy -D warnings` / `cargo test --workspace` /
   `regrtest --check` / `ecosystem --check` all green.

## Drawbacks

- **A stack-switching core is the most dangerous `unsafe` in the
  codebase.** It is small (register save/restore + SP swap), but it
  interacts with `stacker`, unwinding, and the GIL. Mitigations: the
  switch core is confined to one audited module; greenlet stacks
  disable segmented growth; Rust panics are caught at the greenlet
  entry trampoline and converted to Python exceptions (a panic must
  never unwind across a switched stack); the semantics matrix runs
  under the threaded regrtest mode and (advisory) under
  `WEAVEPY_JIT=1`.
- **Shipping a bundled third-party package that shadows PyPI.** The
  numpy-facade precedent exists, but greenlet's dist-info shim means
  `pip install greenlet==<other version>` silently resolves to ours.
  Accepted: the alternative is a guaranteed crash from the cp313
  wheel; the dist-info records the modeled upstream version and
  `greenlet.__doc__`/`notes` say so plainly.
- **Wheel-cache weight and drift.** scipy + matplotlib + lxml wheels
  add ~100 MB per platform to the offline cache, and their pins are
  platform-conditional. Mitigated as in RFC 0056: exact per-platform
  pins in `ecosystem_fetch.py`, online lane non-blocking.
- **Scope risk on the C-API tail.** lxml alone binds more of
  `PyUnicode` than everything currently green combined. Mitigation:
  the rows are independent — WS1/WS2/WS4 are engine work with
  in-tree acceptance, and any of the three WS3 rows can land red with
  a precise reason without sinking the wave (acceptance 3 is the
  goal; the documented fallback is ≥ 2 of 3 green plus an enumerated
  worklist, explicitly flagged for wave 4).
- **The numpy `_core` lane may be slow** at ~8× interpreted. The
  2400 s budget and the `-q`/no-cacheprovider harness settings follow
  the attrs learnings; if it times out, the honest outcome is a
  measured skip with the perf-wave dependency named — but that
  outcome fails acceptance 5, forcing lane-splitting (per-file
  selftest commands) before conceding.

## Alternatives

- **Load the real greenlet cp313 wheel through the binary ABI**:
  rejected — greenlet's C core reads and rewrites CPython
  `PyThreadState` internals (datastack chunks, cframe pointers,
  trash-can state) and memcpy-slices the C stack around CPython's
  frame layout. None of that exists in WeavePy; the wheel cannot even
  import truthfully. Every alternative implementation (PyPy, GraalPy)
  reached the same conclusion and shipped their own.
- **A frames-only greenlet (PyPy's shape) without stack switching**:
  rejected as the primary design — WeavePy's recursive evaluator
  means it cannot switch under interleaved C frames, which excludes
  `sqlalchemy.greenlet_spawn` and gevent, the two consumers that
  justify the work. Considered as a fallback if the switch core
  proves unshippable in-wave; acceptance 4's C-frame fixture exists
  precisely to force the honest choice.
- **Rewrite the evaluator to an iterative heap-frame trampoline**
  (making frames-only greenlets fully general): rejected for this
  wave — it is the "contiguous frame rewrite" the perf waves have
  rejected four times, inverted; a whole-VM control-flow rewrite is
  its own RFC with its own perf story, and stack switching delivers
  greenlet semantics without it.
- **A `_testbuffer` reimplementation in Rust** instead of compiling
  CPython's: rejected — the module *is* the spec for `test_buffer`,
  down to constant values and error strings; the standing policy
  (RFC 0053's dual-truth lesson, RFC 0060's `_testcapi` treatment) is
  verbatim-where-possible.
- **Skip the numpy selftest; add more probe assertions instead**:
  rejected — RFC 0062 named the suite as the capstone for a reason:
  probes assert what we thought to test, upstream suites assert what
  the package's authors know can break. The `installed`-mode runner
  change is small and reusable (it is how scipy/Pillow selftests
  would land in a future wave).

## Prior art

- **PyPy** ships its own greenlet on `_continuation` and treats the
  upstream greenlet test suite as its acceptance harness; its
  compatibility notes (thread-boundness edges, `gr_frame` during
  switching) are the residual map WS4 expects. PyPy also chose
  per-implementation shipping over wheel-loading — the same call this
  RFC makes.
- **GraalPy** maintains patched wheels/overrides for packages whose C
  layers assume CPython internals — precedent for the dist-info shim
  approach to `greenlet`.
- **greenlet upstream** documents its platform switch contracts in
  `platform/switch_*.h` (save callee-saved registers, swap SP, restore
  — nothing else); that minimal contract is what the WS4 switch core
  implements, and `corosensei` (Rust, used in production by Wasmtime
  fibers' lineage) implements the same contract with guard pages.
- **CPython** itself ships `_testbuffer` as the `test_buffer` harness
  — compiling it verbatim continues the RFC 0060 `_testcapi` fixture
  strategy.
- **RFC 0046/0047/0060** (numpy from source → binary wheels → pandas
  capstone) established the "the wheel is the distribution mechanism;
  the upstream suite is the spec" ladder this wave climbs one more
  rung of.

## Unresolved questions

- **corosensei vs. a hand-rolled switch core.** corosensei's
  `Coroutine` model wants to own yield points; greenlet's graph
  switching (any greenlet → any greenlet, not caller/callee pairs)
  may fit its lower-level `Fiber`-style API or may want ~200 lines of
  our own per-platform assembly. Decided by a spike at implementation
  start; the semantics matrix is identical either way.
- **Windows switch support in-wave.** aarch64/x86_64 SysV is
  straightforward; Windows x64 has shadow-space/TEB
  (`StackBase`/`StackLimit`) bookkeeping. If it does not land
  in-wave, the greenlet row gets `status_windows = "skip"` with a
  reason (Windows lanes are still advisory per RFC 0063/0064), and
  the flip-to-blocking wave inherits it.
- **How far the gevent capsule carries.** gevent also wants monkey-
  patchable `socket`/`ssl` internals and its own C event loop
  (`libev`/`libuv` bundled in its wheels). The stretch row measures
  it; full gevent may be a wave-4 headline rather than a residual.
- **numpy `_core` lane runtime** at current interpreter speed —
  measured early (week 1) so lane-splitting can happen in-wave if
  needed rather than at acceptance time.
- **Whether the lxml wheel's libxml2 build assumptions**
  (thread-local error handlers, `PyGILState` around parser callbacks)
  interact with the RFC 0024 GIL model; the row measures it, and the
  `_ssl`/`sqlite3` trampoline precedents suggest the pattern holds.

## Future work

- **gevent as a headline row** (full monkey-patching + its own suite)
  once the capsule surface and any loop-integration gaps from the
  stretch row are enumerated.
- **scipy/Pillow/lxml selftests** via the new `installed` mode — this
  wave proves the probes; their upstream suites are the natural
  wave-4 capstones.
- **scikit-learn** (scipy + Cython + joblib/loky process pools) as
  the next matrix rung once scipy is green.
- **A fiber-aware PEP 703 design note**: the WS4 switch core creates
  per-stack execution contexts; the free-threading RFC should say
  whether they become per-thread, per-fiber, or both.
- **`test_capi` buffer legs**: with `_testbuffer` in-tree, the
  buffer-adjacent `_testcapi` submodules become reachable for the
  conformance-zero wave.
