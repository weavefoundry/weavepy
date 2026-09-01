# RFC 0076: The convergence wave — ecosystem selftest zero, performance wave 11, experimental free-threading, and the 3.14 horizon

- **Status**: Draft
- **Authors**: WeavePy authors
- **Created**: 2026-08-28
- **Tracking issue**: TBD
- **Builds on**: RFC 0074 (whose measured wave-11 charter — closure
  cells, escaping callees, list shapes, object-lane truthiness — this
  wave executes), RFC 0075 (the embedding surface PEP 741 extends, the
  scipy/Pillow/lxml selftest lanes this wave burns, and the real
  per-thread states the free-threading mode inherits), RFC 0072/0066
  (the selftest-lane protocol and the free-threading design-note debt,
  owed since wave 3 of the ecosystem tier), RFC 0068 (the 3.14
  gap-analysis debt, owed since conformance zero), RFC 0058 (the
  measured-bench protocol and committed-baseline discipline), RFC
  0049 (measured-baseline protocol for expectations rewrites).

## Summary

Every scoreboard WeavePy keeps is green except four rows, one number,
and two IOUs. The regrtest sweep grades fail 0 / unexpected 0 across
550 labels; all 42 ecosystem probe rows pass; but the **selftest tier**
— running packages' *own* test suites — still carries four measured
`fail` rows (numpy, scipy, Pillow, lxml) and one speed-bound `skip`
(attrs); the bench suite sits at **2.85× CPython geomean** against a
chartered ≤ 2.2×; and two design debts have been rolled forward
wave over wave: the **free-threading / PEP 703 design note** (owed
since RFC 0066) and the **CPython 3.14 gap analysis + PEP 741** (owed
since RFC 0068).

This wave converges all four fronts in one landing:

1. **Pillar I — ecosystem selftest zero (WS1–WS5).** Burn the four
   selftest `fail` rows to measured `pass`-with-enumerated-deselects
   (numpy's residual 21-module census, scipy's two root causes,
   Pillow's ~10 deltas, lxml's ~44), un-skip the attrs selftest on the
   faster interpreter Pillar II delivers, and grow the matrix with the
   shapes it has never seen: **polars** (the first big PyO3/abi3
   Rust-native consumer), **psycopg2**, **celery**, **uvicorn** as a
   real ASGI server row, **alembic** migrations over sqlalchemy, and a
   **torch** (CPU) capstone.
2. **Pillar II — performance wave 11 (WS6–WS9).** Execute the RFC 0074
   charter against the fresh `deltablue` census: closure-cell lanes
   (`LOAD_DEREF`/`STORE_DEREF`), escaping callees + per-kind `CallDyn`
   fast paths (builtin fast-call, bound-method direct entry,
   `CallDynKw`), heterogeneous `BUILD_LIST` shape lanes, `set` lanes,
   object-lane truthiness (`TO_BOOL`), the `LOAD_ATTR` probe-miss
   residue, and the twice-deferred allocation elision + generator
   guard epochs. Gate: suite geomean **≤ 2.2×** CPython.
3. **Pillar III — experimental free-threading (WS10–WS12).** Pay the
   PEP 703 debt with substance, not prose: a checked-in design note
   grounded in WeavePy's already-atomic `sync::Rc = Arc` heap, an
   **experimental `-X gil=0` / `PYTHON_GIL=0` runtime mode** (single
   binary, no second ABI) with per-object critical sections and
   container-lock discipline, CPython 3.13t's extension-compatibility
   contract (non-declaring C extensions re-enable the GIL with a
   `RuntimeWarning`), and a scoped free-threaded conformance lane.
4. **Pillar IV — the 3.14 horizon (WS13–WS15).** A **measured** 3.14
   gap analysis (vendor the 3.14 `Lib/test`, sweep it once, enumerate
   the delta as the upgrade wave's charter), **PEP 741
   `PyInitConfig`** over the RFC 0075 config core, and the additive
   3.14 features that don't fork the 3.13 target: `compression.zstd`
   (PEP 784) ungated, and PEP 750 t-strings + PEP 758 bare
   `except`/`except*` behind an explicit `-X lang=next` preview gate.
   The pillar also **adopts a standing version policy**: WeavePy
   tracks the latest stable CPython minor on a single-version trunk,
   switching when N.1 ships and the matrix's cp31N wheels exist —
   which makes the 3.14 switch the committed next wave (RFC 0077),
   chartered by this wave's measured gap analysis.

As always, the deliverable is measured: the full sweep re-runs at
`unexpected 0`, every touched expectations row is rewritten from
evidence, the bench baseline is re-committed from fresh runs, and the
free-threaded lane lands with its own measured expectations file.

## Motivation

1. **The selftest rows are the drop-in claim's last measured red.**
   A probe proves a package *imports and works once*; a selftest
   proves WeavePy runs the package the way its own maintainers demand.
   numpy/scipy/Pillow/lxml are the four most-depended-on native
   packages on PyPI, and their `selftest_status = "fail"` rows are
   the only place in the repo where a measurement currently says "not
   yet a drop-in". Every enumerated cluster (3-arg `matmul` dunder
   arity, builtin `__module__` pickling, Cython's 3-arg raise, the
   Pillow `getim` capsule edge, ResourceWarning finalizer discipline)
   is an engine-correctness fix that outlives the row that found it.
2. **Wave 10 measured flat; the charter is already written.** RFC
   0074 landed its lanes but held at 2.86× because the gated fixtures
   reject on the *next* shape stratum, which its closing census
   enumerated precisely: `LOAD_DEREF`, escaping callees, `BUILD_LIST`
   shapes, `TO_BOOL`, `LOAD_ATTR` residue. Executing a measured
   charter is the cheapest perf work there is — and it is also
   ecosystem work: the attrs selftest skip and six of numpy's
   budget-bound selftest modules are *interpreter-speed* rows that
   flip only when the interpreter gets faster.
3. **The free-threading note has been owed for ten RFCs, and the
   window is now.** RFC 0075's WS3/WS4 built the exact prerequisites
   the note kept waiting on: a real lifecycle state machine and real
   per-thread states. Meanwhile CPython 3.14 made free-threading
   officially supported (PEP 779) and the ecosystem is starting to
   ship `cp314t` wheels. WeavePy's heap is *already* atomically
   refcounted (`sync::Rc = Arc` since the RFC 0024/0025 cross-thread
   waves) — the GIL protects interpreter services and object
   *internals*, not refcounts — which makes an experimental no-GIL
   mode a locking-discipline project, not a heap-rewrite project.
   That is a structural advantage over CPython worth measuring, and
   the longer it goes unmeasured the more the design note drifts
   toward fiction.
4. **"Drop-in for CPython" is starting to mean 3.14.** The 3.13
   branch is deep in bugfix-only; pip, setuptools, and the big wheels
   all publish cp314 first now. WeavePy does not need to *switch*
   targets this wave — the 3.13 surface is the measured asset — but
   it needs to know the size of the delta (grammar, bytecode, stdlib,
   C-API) from a sweep, not from release notes, and it should land
   the additive pieces (PEP 741, zstd) while they are cheap.
5. **Cost of inaction.** Each pillar left unworked compounds: red
   selftest rows drift as upstreams release, the perf charter's
   census goes stale, the free-threading note's prerequisites decay
   into archaeology, and the 3.14 delta only grows.

## CPython reference

- **Package suites under test (Pillar I)**: numpy 2.5.x
  (`numpy._core` + top-level suites; residual census in
  `tests/ecosystem/expectations.toml`), scipy (linalg.interpolative's
  40-failure cluster, fft pickling, Hadamard ordering), Pillow
  (`Tests/`, sdist mode), lxml (overlay mode per RFC 0075 WS9), attrs
  (hypothesis-driven), polars (abi3 wheel over PyO3), psycopg2
  (libpq C extension), celery (memory transport), uvicorn (asyncio
  ASGI server, h11), alembic (sqlalchemy migrations), torch CPU
  (`torch`, autograd, `torch.utils.data` with worker processes).
- **PEP 703** (Making the GIL optional), **PEP 779** (free-threading
  officially supported, 3.14), `Python/ceval_gil.c` and
  `Include/internal/pycore_critical_section.h`: the
  `Py_BEGIN_CRITICAL_SECTION` per-object locking protocol, the
  `PYTHON_GIL` env var and `-X gil` semantics, `sys._is_gil_enabled()`,
  and the module-slot contract `Py_mod_gil` /
  `Py_MOD_GIL_NOT_USED` — an extension that does not declare support
  re-enables the GIL at import with a `RuntimeWarning`
  (3.13t behavior, kept in 3.14).
- **PEP 741** (Python configuration C API, 3.14):
  `PyInitConfig_Create/Free`, `PyInitConfig_Set{Int,Str,StrList}`,
  `PyInitConfig_GetError`, `Py_InitializeFromInitConfig`, and the
  runtime get surface (`PyConfig_Get`, `PyConfig_GetInt`,
  `PyConfig_Names`) — a stable-ABI, string-keyed front over the PEP
  587 structs RFC 0075 shipped.
- **PEP 750** (template strings, 3.14): the `t"..."` prefix,
  `string.templatelib.Template`/`Interpolation`, tokenizer and
  grammar changes in `Lib/test/test_tstring.py`.
- **PEP 758** (allow `except`/`except*` without parentheses, 3.14):
  grammar-only, `test_grammar`/`test_syntax` deltas.
- **PEP 784** (`compression.zstd`, 3.14): the `compression` namespace
  package re-exporting `lzma`/`bz2`/`zlib`/`gzip` plus the new
  `_zstd` bindings (`ZstdCompressor`, `ZstdDecompressor`, `ZstdFile`,
  `train_dict`), `Lib/test/test_zstd.py`.
- **CPython 3.14 `Lib/test`** (vendored at 3.14.x for WS13's one-off
  gap sweep): the authoritative enumeration of everything else —
  PEP 649/749 deferred annotations (`annotationlib`), the 3.14
  bytecode/magic delta, `test_free_threading/`, stdlib
  removals (PEP 594 stragglers) and additions.
- **RFC 0074's closing census**: the wave-11 charter this RFC's
  Pillar II executes verbatim (`LOAD_DEREF`, `CALL (callee escapes)`,
  `BUILD_LIST (shape)`, `TO_BOOL lane`, `LOAD_ATTR shape` residue),
  plus its Future-work list (per-kind `CallDyn` fast paths,
  allocation elision, generator guard epochs, `set` lanes).

## Detailed design

### Pillar I — ecosystem selftest zero

#### WS1 — the numpy residual burn

The RFC 0075 burn took numpy's census from 281 failures to ~74 fail +
~11 error across 21 modules, six of them budget-bound. This wave
finishes it, in enumerated-cluster order:

1. **Dunder arity** — `numpy.matmul`'s 3-arg form (the `out=`
   positional leg) rejects through the VM's binary-dunder bridge;
   the capi `nb_matrix_multiply`/ufunc dispatch must accept the
   ternary shape CPython's slot wrappers do.
2. **Builtin `__module__` pickling** — pickling a builtin/ufunc by
   qualified name fails because C-created callables report a
   `__module__` the reduce path can't re-import; land CPython's
   module-name resolution order (`__module__` from the method def's
   containing module object, falling back to the type).
3. **Cython 3-arg raise** — `raise T, v, tb` shapes emitted by Cython
   generators route through `PyErr_Restore` with a mismatched
   normalization; match CPython's lazy-normalization contract.
4. **ResourceWarning finalizer discipline** — tests asserting
   warnings from unclosed objects need the RFC 0068 FOR_ITER-style
   prompt-drop discipline extended to the remaining temporary shapes
   the suite exercises.
5. **The six budget-bound modules** ride Pillar II: they re-measure
   under the wave-11 interpreter and either fit the existing budget
   or get sharded per the RFC 0075 WS8 mechanism.

Acceptance: `selftest_status = "pass"` with every surviving deselect
carrying a measured reason — or the documented fallback, a `fail`
row whose census is fresh and whose clusters name *new* root causes.

**Measured outcome (2026-08, this wave).** The burn landed well past
the enumerated clusters. Modules taken from red to fully green:
`test_numeric` (was 19 failed — canonical `Mul` sequence-repeat
fallback + error message, `round(s, ndigits=None)` clinic binding,
`PyUnicode_Tailmatch`/`PyUnicode_Find` negative-index adjustment,
member descriptors on foreign-backed dtype classes; the one residual,
`TestClip::test_clip_property`, is a hypothesis `too_slow` health
check — interpreter speed, not correctness), `test_regression` (was
7 — `Py_BuildValue` `c`/`C` format codes for the chararray pickle,
`test_object_array_refcounting` via the surplus-C-refs accounting
below), `test_conversion_utils` (9), `test_defchararray` (6),
`test_array_coercion` (5), `test_nep50_promotions` (4 —
`checked_big_to_double` OverflowError + the 0/±1-base huge-exponent
short-circuit), `test_mem_policy` (9 errors — `os.fsync` for meson's
coredata save, `LIBPYTHON=""` in sysconfig), `test_multithreading`
(PEP 688 `__buffer__`/`__release_buffer__` shims +
`PyMemoryView_FromObjectAndFlags`), `test_longdouble`
(locale-independent `PyOS_string_to_double`/`PyOS_double_to_string`),
and `test_arrayprint` (the `_recursive_guard` refcount test — a
`StoreDeref` that displaces a GC-tracked self-referential closure now
prompt-reaps it, mirroring the frame-teardown sweep, so `recurser =
None` in numpy's `finally` releases the array's C reference without a
cycle collection). `test_umath` now *completes* in ~7 min (was
budget-bound) at 4680 passed / 2 failed, and `test_nditer` is 930
passed / 1 failed after three fixes: `sys.getrefcount` now adds the
faithful body's **surplus raw C refs** (`ob_refcnt` beyond the pin —
an extension's inline `Py_INCREF`, e.g. `NpyIter_Copy`, mints no `Rc`
clone), `PyErr_WriteUnraisable` prints the CPython-default unraisable
report instead of swallowing it, and C getset descriptors gained
their `PyGetSetDef.doc` (`__doc__`), a live read-through of the C
`doc` slot written post-harvest by `add_docstring`, and a deleter
(CPython's `getset_set` with `value == NULL` — `del np.add.__doc__`).
Remaining documented deltas (4): `test_iter_object_arrays_conversions`
(exact refcount of an `int` element — WeavePy ints are unboxed
immediates, architectural), `test_out_wrap_no_leak` (raw C-ref
imbalance on the ndarray-*subclass* wrap path, +3/ufunc-call, now
*visible* through the surplus accounting; pre-existing, tracked for
wave 7), `test_ufunc_docstring` (`np.add.__dict__` completeness —
foreign attribute writes land in a side registry, not the C instance
dict), and `test_clip_property` (hypothesis health check, speed).

The budget-bound census (cluster 5) is landing on the wave-11
interpreter: `test_multiarray` — never graded before; RFC 0075 killed
it at its budget — now *completes* at **14246 passed / 22 failed**
(4 skipped) in 1:18:32, reproduced identically by a logged rerun.
The 22 triage into five clusters plus singletons:

- **memoryview/buffer bridge (11)** — the dominant family:
  `np.array(memoryview(strided_view), copy=True)` scrambles data
  (strides misapplied on a non-contiguous re-export;
  `TestArrayCreationCopyArgument::test_order_mismatch` ×8),
  `copy=False` over a buffer object copies instead of aliasing
  (`may_share_memory` False — `test_buffer_interface`), and the
  `TestNewBufferProtocol` round-trips (`test_roundtrip`,
  `test_ctypes_struct_via_memoryview`) disagree on strided/struct
  layouts. One lane: the VM↔C `Py_buffer` re-export of
  non-contiguous shapes.
- **C-refcount visibility (4)** — `resize` must raise ValueError
  while other references exist (`TestResize` ×3) and
  `__array_finalize__`'s error path asserts "no references should
  remain" (`test_lifetime_on_error`); both read `ob_refcnt`, which
  under the mirror shows the pin, not the VM `Rc` census — the
  `test_out_wrap_no_leak` family, tracked with it.
- **intp converter error shapes (2)** — the deliberately-broken
  `__index__` fixtures produce our dunder-shim's message
  ("__index__ returned non-int…") where the test asserts numpy's
  converter-path wording.
- **fromfile on a dup'd-closed fd (2)** — an OverflowError
  ("Python int too large to convert to C long") escapes from the
  fd/offset conversion before the intended OSError.
- **singletons** — `test_non_sequence_sequence` (a `__len__`-raising
  object should still coerce through the sequence fallback),
  `test_mmap_close` (closing an mmap with a live ndarray view must
  raise BufferError — exporter accounting), and
  `TestUnicodeEncoding::test_round_trip`, *half-fixed this wave*:
  `unicodedata.normalize` now accepts str subclasses (`numpy.str_`;
  CPython's `PyUnicode_Check` is subtype-inclusive — canary in
  `test_rfc0076_burn_regressions.py`), leaving only lone-surrogate
  fidelity through the UCS4 crossing (`'\ud800'` → U+FFFD).

`test_dtype` re-measures at 1176 passed / 3 failed (2:30) — the two
`TestDTypeMakeCanonical` hypothesis legs plus `test_structured`, to
triage. `test_strings` first completed at 1966 passed / 49 failed
(3:37:46), the tail uniformly the partition/rpartition unicode
cases — which turned out not to be a partition bug at all: the
results were correct, and the tests' round-trip *verification*
(`act1 + act2 + act3`, an `np.str_` plus an ndarray) raised through
WeavePy's operator dispatch. CPython's `binary_op1` consults only
the *number* slots — `sq_concat`/`sq_repeat` are a `PyNumber_Add`
last resort — so a str subclass's inherited `str.__add__` wrapper
(which raises "can only concatenate str …" rather than returning
NotImplemented, faithfully) must not pre-empt the partner's
reflected dunder; the VM's generic pass now defers an inherited
*builtin* sequence wrapper on Add/Mult until after the reflected
pass when the operands are different classes. The partition subset
re-runs **82/82** (10 s, down from 97 s), and the full-module
re-census lands at **2011 passed / 4 failed** (2:05:39) — the four
being `test_istitle_unicode` on U+1FFC, traced to
`_PyUnicode_IsTitlecase` being hardwired false (numpy's C
`Py_UNICODE_ISTITLE` loop denied every category-Lt leader); it now
reads the VM's UCD title flag and the istitle subset re-runs
**50/50**, so the module's residual is **0**. Canaries bundled in
`test_rfc0076_burn_regressions.py` (§5–§6). The same sweep
half-fixed `TestUnicodeEncoding` (see the singleton above).

The purged-lane re-sweep landed: `test_mem_overlap` is **clean**
(25 passed / 0 failed, 36:40) and `test_scalarmath` is one off
(**1580 passed / 1 failed**, 4:39 — `test_array_scalar_ufunc_dtypes`).
`test_stringdtype` grades **2549 passed / 399 failed** (33:00): the
dominant cluster is one family — `StringDType(coerce=False)`'s
C-side string check rejects values crossing WeavePy's bridge
("StringDType only allows string data when string coercion is
disabled") — fanning out across the parametrized `test_binary`
method matrix; a follow-up burn target, not 399 root causes.

#### WS2 — the scipy burn

46 failures, two real root causes plus a singleton:

- **`scipy.linalg.interpolative` (40)**: one cluster — the
  Fortran-backed ID decomposition's RNG-seeding path hits a
  `PyArray_*` scalar-coercion edge; expected single-fix.
- **fft pickling (5)**: `scipy.fft`'s pickled plan objects hit the
  WS1 builtin-`__module__` fix; expected to flip with it.
- **Hadamard ordering (1)**: a sequency-ordering assertion; root
  cause TBD from the failing test, enumerated if upstream-shaped.

*Burn outcome (measured)*: all 46 deltas fixed (the linalg+fft lane's
only remaining failure, `test_decomp.py::TestEig::test_singular`,
fails identically on the CPython 3.13 baseline — a LAPACK
environment shape, delta count **0**). Three root causes, none the
predicted ones:

- **`interpolative` (40)** was `PyNumber_MatrixMultiply` — the
  Cython backend's `A @ B` reached a bespoke path that looked up
  `__matmul__` (bound, for an instance receiver) and passed the
  receiver *again*, so ndarray's 2-arg `__matmul__` saw three
  arguments. Now routed through the same slot/VM `binop` dispatch as
  every other operator (`nb_matrix_multiply` for foreign operands,
  `BinOpKind::MatMult` for native ones), with
  `PyNumber_InPlaceMatrixMultiply` riding `nb_inplace_matrix_multiply`
  through the shared in-place helper.
- **fft/multiprocess pickling (5)** was the builtin-`__module__`
  *store*, not the fix's absence: `BUILTIN_WRITABLE_MODULE` was
  thread-local, so numpy's import-time
  `_reconstruct.__module__ = 'numpy._core.multiarray'` was invisible
  to the `multiprocessing.Pool` task-feeder thread and `whichmodule`
  fell back to the extension's short `m_name`. Now process-global,
  like the other descriptor registries.
- **Hadamard (1)** was `math.log(4, 2)` returning
  `1.9999999999999998`: `loghelper` used the `_PyLong_Frexp`
  decomposition for *every* int, where CPython converts to double
  first and only decomposes on overflow — one ULP off for exact
  powers of two, so `hadamard(n)`'s power-of-2 validation rejected
  legitimate sizes.

The row's scope (`--pyargs scipy.linalg scipy.sparse scipy.fft`)
grows `scipy.special scipy.ndimage` if the Pillar II interpreter
brings the measured runtime under budget (the RFC 0075 unresolved
question, now answerable).

#### WS3 — the Pillow and lxml burns

- **Pillow (~10 deltas)**: the `getim` capsule edge (the legacy
  `PyCapsule` image-pointer surface), buffer-protocol strides on
  mode-`P` images, and ResourceWarning legs shared with WS1(4).

  *Burn outcome (measured)*: 4846 passed, 1 failed — and the one
  failure (`test_imagegrab.py::test_grab`, a screen-capture
  subprocess) fails identically on the CPython 3.13 baseline, so the
  delta count was **0**. A post-burn re-measure surfaced one
  regression, `test_font_leaks.py::TestDefaultFontLeak::test_leak`
  (RSS ceiling 1 MB over 100 `draw.text` calls): every fresh ~10 KB
  text marshaled into `_imaging`'s bitmap-font path leaked ~60 KB —
  `PyUnicode_AsLatin1String`'s bytes result rides the always-pinned
  bytes mirror lane (RFC 0066 WS7), the scalar-pin cache's only
  eviction was the 64Ki-*entry* high-water mark, and the
  `PyUnicode_AsUTF8` C-string cache pinned a *fresh copy per call*
  forever. Both caches are now byte-accounted (512 KB payload HWM
  each): the pin cache sweeps dead entries when payload bytes cross
  the mark (tuples charged for the element mirrors they retain), and
  the C-string cache dedupes by `Rc` identity with a `Weak` liveness
  handle, dropping buffers whose string has died — CPython's "valid
  while the object lives" contract. Growth measured ~2.5 MB → ~0.5 MB
  per 50 calls; the final lane run is **4850 passed / 0 failed** (312
  skipped, 3 xfailed, 40:30) — grab included — a genuine zero.
  The load-bearing fixes beyond the WS3/lxml
  batch: a dedicated `PyCapsule` builtin type so `type(im.getim())`
  reports `<class 'PyCapsule'>`; `PyNumber_Index`'s TypeError naming
  the offending type (ImageCms asserts the `'NoneType' object cannot
  be interpreted as an integer` shape); `Py_BuildValue("HH", …)`
  (unsigned short — `getextrema` on `I;16` images returned a
  `(None, None)` tuple through the forgiving default);
  `PyUnicode_AsLatin1String` as a *strict* codec raising
  `UnicodeEncodeError` instead of `'?'`-substituting (bitmap-font
  `getbbox` on non-latin-1 text, Pillow issue #2826); and — the
  ResourceWarning leg, WS1(4)'s discipline — `prompt_reap_dropped`
  probing dying untracked *instances* for anchored tracked children,
  so a handled exception displaced by the compiler's `e = None; del
  e` epilogue (its actual death site — POP_EXCEPT runs while `e`
  still binds it) cascades through `__traceback__` → frame → locals
  and frees the plugin-candidate instances `Image.open` discarded,
  which were the sole holders of the still-open file (the
  mpo/psd/spider/tiff `test_unclosed_file` quartet fired their
  warning one amortized-sweep too late once the heap crossed the
  8192-tracked-object stride threshold).
- **lxml (~44 deltas)**: doctest-heavy; the census clusters around
  error-message exactness in the pyexpat/libxml2 bridge and
  tp-richcompare edges on proxy elements. Burn by cluster; enumerate
  the upstream-shaped remainder (lxml asserts libxml2-version-
  specific strings) as deselects with reasons.

  *Burn outcome (measured)*: 44 deltas → 0 against the CPython 3.13
  baseline. The load-bearing fixes, in burn order: traceback/frame
  type identity in `type_for_object` (Cython 3-arg `raise`);
  dunder-shim precedence honouring `tp_methods` entries already in
  `tp_dict`; `dict.pop` arity; pyexpat buffer-protocol (`y#`) parse
  args; `PySlice_Unpack` overflow clamping; faithful-list mirror
  reconciliation for *never-registered* mirrors via a mint-time
  agreement snapshot (`MirrorPrefix::list_mint` — Cython's inlined
  `list.pop()` on `lxml.sax`'s `_element_stack`); `note_c_agreement`
  recording the `Rc` fingerprint alongside the buffer snapshot so an
  append→`del path[-1]`→append cycle can't alias a stale fingerprint
  and swallow the delete (`descendantpaths()` accumulation);
  `__delattr__` shim derived from `tp_setattro` (CPython
  `add_operators` parity — `del root.c1` on objectify); `dir()`
  dispatching a C type's `__dict__` property override (objectify's
  fake child dict); and ctypes `c_char_p`/`c_wchar_p` arguments
  marshalling through a process-lifetime interned buffer (CPython
  passes a pointer into the bytes object itself — lxml's
  `adopt_external_document` `strcmp`s a capsule context stored one
  call earlier).

#### WS4 — un-skipping the attrs selftest

The row's spec never left the manifest; the skip reason is
"interpreter-speed-bound" with a measured >5-minute single test.
After Pillar II lands, re-measure `tests/test_make.py`,
`test_funcs.py`, `test_dunders.py` under the 2400 s budget. If the
suite fits, the row flips to a measured verdict; if hypothesis remains
disproportionate, the fallback is a `@given`-deadline profile
(`HYPOTHESIS_PROFILE=weavepy` with reduced `max_examples`), recorded
in the row comment — a *scoped* run beats a skip.

*Burn outcome (measured)*: the row flips to a measured **pass** — the
full suite (no hypothesis profile trim) runs **1379 passed / 0
failed** (6 skipped, 1 xfailed) in **17:20**, comfortably inside the
2400 s budget that the wave-4 interpreter blew (killed at 2403 s with
~1385 tests collected and the tail unreached). Two engine fixes were
load-bearing before the clean run, both surfaced by the
`cached_property`-on-`__slots__` tests: zero-arg `super()` inside a
method compiled by an *enclosing function* (attrs generates
`def wrapper(_cls): __class__ = _cls; def __getattr__(self, …): …
super() …` and `compile()`s it at class-build time) must resolve
`__class__` through normal lexical scoping — the compiler now surfaces
`__class__` as needed-from-outer when an inner function reads `super`,
so the wrapper's plain local is promoted to a cell *before* emission
(previously: "super(): `__class__` cell not found", then post-gate-fix
"cannot access free variable '__class__'"). Canaries live in
`tests/regrtest/test_rfc0076_burn_regressions.py`.

#### WS5 — new rows: polars, psycopg2, celery, uvicorn, alembic, and the torch capstone

- **polars**: the matrix's first large PyO3/abi3 consumer — the
  Rust-native analogue of what grpcio was for C++. Probe:
  `DataFrame` construction, `group_by().agg()`, lazy-frame
  `collect()`, join, parquet round-trip (exercises the abi3 buffer
  and `PyBytes` surfaces), and `map_elements` with a Python callable
  (the Rust→Python re-entry leg). polars drives the *stable* ABI
  hard; NULL-stub dyld gaps surface here first.
- **psycopg2**: the RFC 0072 deferral, closed. C extension over
  libpq, sdist-built in the offline lane (the RFC 0062 C-sdist path).
  Probe mirrors the psycopg (v3) row's serverless posture:
  adaptation/`sql.SQL` composition, `connection`-class surface,
  error taxonomy — no live server required.
- **celery**: probe over the in-process `memory://` transport +
  cache backend: define a task, start a worker thread
  (`worker_main` in a thread with `--pool=solo`), submit, assert the
  result round-trip and clean shutdown. Exercises kombu's event
  loop, billiard's process shims, and vine promises — pure-Python
  but concurrency-shaped.
- **uvicorn**: the deployment twin of RFC 0075's gunicorn capstone —
  launch `uvicorn` serving the RFC 0060 FastAPI app as a real
  process, drive concurrent HTTP over loopback (h11 leg), assert
  responses, SIGTERM, and clean-exit. This graduates FastAPI from
  `TestClient` to the shape people actually deploy.
- **alembic**: `alembic init` + autogenerate a revision against a
  sqlalchemy model over sqlite + `upgrade head` + `downgrade -1`,
  asserting schema state each step. Exercises the migration-script
  exec path (compile + exec of generated files) end-to-end.
- **torch (CPU) capstone**: the heaviest wheel on PyPI, resolved via
  the existing cp313 tags. Probe: tensor construction +
  `matmul` cross-checked against numpy, an autograd
  `backward()` gradient check, a three-epoch MLP training loop on
  synthetic data asserting monotone loss, `state_dict()`
  save/load round-trip, and a `DataLoader` with `num_workers=2`
  (the multiprocessing leg). `torch.compile` is out of scope (it
  requires a host toolchain and is gated upstream); the row comment
  says so. Budget and wheel-cache growth (~200 MB Linux, ~70 MB
  macOS arm64) get their own manifest entries per the RFC 0056
  posture.

Rows land in `manifest.toml` + `expectations.toml`;
`tools/ecosystem_fetch.py` learns the pins; CI cache keys follow the
manifest hash.

**Measured outcome (macOS arm64).** All six rows pass. The torch
burn was the deep one — the fix ledger, in landing order: metatype
allocation sizing in `PyType_Ready`/`PyType_GenericAlloc` (pybind11
overflowed an undersized metatype box); `tp_dict` publication for
VM-backed types (a NULL `tp_dict` broke `PyDict_Merge` during
`_initExtension`); foreign-descriptor `__get__`/`__hash__` binding
(pybind11 `instancemethod` and static properties); byte-faithful
`PyGetSetDescrObject` boxes (`add_docstr` type-checks
`getset_descriptor` by name); metaclass-drift adoption in
`PyType_Ready` plus the VM-side drift hook (`OpaqueBaseMeta`);
tuple-subclass seeding + faithful `tuple_new` + a direct
`tuple_mp_subscript` (`torch.Size` construction, `len`, pickling,
and subscripting without slot re-dispatch recursion); module
`__class__` reassignment (`torch._dynamo`'s `ConfigModule`);
`__text_signature__` for `_operator` builtins; PEP 604 union
hashing; `_lru_cache_wrapper` as a type; `chain.from_iterable` as a
METH_CLASS builtin; CPython-exact `struct` signatures;
teardown-tolerant TLS in forked DataLoader workers; executable-bit
restoration in `_minipip`/`zipfile` (`torch_shm_manager`); and a
tier-2 JIT fix — the obj-global helper pinned a `None` snapshot as a
regular pin index, so a native `is None` fence read it as non-None
(`_cupti_monitor.push_user_annotation`); the helper now answers the
object lane's nullable `-1` and deopt moved to `-2`
(`test_jit_object_lanes.py` regression). uvicorn's probe asserts
`-SIGTERM` + the graceful-shutdown log lines; celery's wraps
`result.get()` in `allow_join_result()`; psycopg2 needed extension
`__name__` = spec name, exception `tp_traverse`/`tp_clear`, and
probe-side adaptation-surface corrections.

### Pillar II — performance wave 11

**Affected crates**: `weavepy-jit` (`analyze.rs`, `ir.rs`,
`lower.rs`, `runtime.rs`), `weavepy-vm` (`tier2.rs`). No bytecode or
object-model-layout changes.

#### WS6 — closure-cell lanes

`LOAD_DEREF`/`STORE_DEREF` compile: cells ride the wave-7 nullable
object-lane pin discipline (a cell is an object slot whose payload is
re-read per access — no burn-in, because closures exist to be
mutated), with a fast integer lane for cells whose observed payload
is unboxed-stable under the standard type guard.
`MAKE_CELL`/`COPY_FREE_VARS` frame setup joins the native prologue so
closure-*defining* frames compile, not just closure-calling ones.
This is `deltablue`'s (22.4×) and `richards`'s (11.7×) named front
line.

#### WS7 — escaping callees and per-kind CallDyn fast paths

- **`CALL (callee escapes)`**: a callable loaded from a container or
  attribute (the census's `callables stored into containers/
  attributes`) currently rejects the frame. It becomes a `CallDyn`
  admission: the callee value rides an object pin and dispatches
  through the opaque-call lane.
- **Per-kind `CallDyn` fast paths** (the RFC 0074 Future-work head,
  against its landed counters): builtin fast-call skips the
  interpreter prologue for `METH_FASTCALL`/`METH_O` targets;
  bound-method direct entry splits the receiver and enters the
  function body natively when it is itself compiled; **`CallDynKw`**
  admits keyword call sites through the kwnames protocol instead of
  rejecting.

#### WS8 — collection shapes and object-lane truthiness

- **Heterogeneous `BUILD_LIST` shape lanes**: element lanes per
  slot (int/float/object mix) instead of the current homogeneous
  requirement — `list_ops` (13.1×) and `nbody` (10.6×) reject here.
- **`set` lanes** over the wave-9 receiver-agnostic native-method
  machinery: construction, `add`/`discard`/membership, and the
  fused `for x in set` loop.
- **`TO_BOOL` object lane**: truthiness on object-lane values lowers
  to the type-guarded fast paths (`None` → false via the `-1` pin
  encoding, bool identity, int/str/list/dict emptiness) with a
  generic `__bool__`/`__len__` helper fallback instead of a frame
  reject.
- **`LOAD_ATTR shape` residue**: the remaining probe-miss classes
  from the wave-10 census (shadowed slots, version-tag misses on
  long-lived instances) resolve through the generic helper rather
  than rejecting.

#### WS9 — allocation elision and generator guard epochs

- **Allocation elision** (deferred from wave 9, prerequisites landed
  in wave 10): tuples and argument packs that never escape the
  compiled region are SROA'd into lanes; the deopt path
  materializes them on demand (the same displaced-value discipline
  attribute stores use). Candidates re-profiled fresh; `pyaes`
  (12.2×) and `fannkuch` (9.2×) are the named beneficiaries.
- **Generator guard epochs (Phase B)**: parked native generators
  survive world changes by epoch-stamping their guards instead of
  invalidating wholesale — resumes revalidate one epoch counter.

> **Landing note (honest miss, third deferral).** WS9 did not land
> in this wave; both halves were investigated and measured out.
> *Guard epochs*: a sound epoch requires every namespace mutation
> (globals/builtins dict stores, type-dict writes, callee rebinds,
> `math`-module stores) to bump a counter the resume path can check;
> today `DictData` carries no version word and the only versioning
> in the runtime is `specialize.rs`'s per-class `attr_version`.
> Retrofitting a global mutation-tracking layer mid-wave — under a
> wave that also touches every container's mutation path for
> `gil=0` — was judged an unacceptable soundness risk against a
> resume-time revalidation (`guards_hold`) that profiles cheaply.
> *Allocation elision*: the wave-10 displaced-value discipline
> covers attribute stores, but tuple SROA additionally needs an
> escape analyzer over the IR and per-deopt materialization maps —
> infrastructure with no other consumer yet. The WS6–WS8 lanes
> (closure cells, escaping callees, CallDyn fast paths, list/set
> shape lanes, `TO_BOOL`, `LOAD_ATTR` residue) landed in full and
> carry the wave's perf delta; the elision/epoch pair rolls forward
> with its census intact, per the RFC 0074 honest-miss protocol.

**Gate**: suite geomean **≤ 2.2×** CPython on the committed
macOS-aarch64 baseline, with the RFC 0074 criterion-2 per-fixture
floors carried forward (`deltablue` ≥ 2.5× improvement vs the wave-9
committed row, `richards` ≥ 2.0×, `call_overhead` ≥ 1.8×, `list_ops`
≥ 1.6×, `dict_ops` ≥ 1.5×, `pyaes` ≥ 1.5×, `str_methods` ≥ 1.4×,
`nbody` ≥ 1.4×, `fannkuch` ≥ 1.3×, `json_bench` ≥ 1.3×); loop
kernels hold ≤ 0.06×; no fixture regresses outside its committed
envelope. Wave 10's honest-miss protocol applies: if a gate is
missed, the closing census enumerates the *new* front line and the
baseline is re-committed from what measured.

> **Landing note (measured outcome, honest miss).** The gate was
> missed: the suite closed at **3.02× CPython geomean** (5 samples,
> macOS-aarch64, baseline re-committed from what measured, per the
> protocol above). Two findings from the closing measurement:
>
> 1. **The escaping-callee lane needed a backoff.** WS7's `CallDyn`
>    admission compiles call-shaped frames whose callees are *not*
>    natively enterable — every such call pays the activation-shell
>    round-trip plus a full `guards_hold` snapshot re-validation the
>    interpreter never would. On `deltablue` this made the compiled
>    kernel a net **25% loss** against tier-1. The fix mirrors the
>    deopt budget: per-code `generic_dyn_calls` / `native_entries`
>    counters (surfaced as `generic dyn calls` / `generic-call
>    retirements` in `WEAVEPY_VM_STATS`), and a frame averaging
>    ≥ 4 generic interpreter round-trips per framed entry (after 64
>    entries) retires to `NotJitable` exactly as the deopt budget
>    retires chronic side-exiters. With the backoff, `deltablue`
>    with the JIT sits at parity-or-better vs tier-1 (432ms vs
>    447ms timed region) instead of 25% behind, and the suite
>    recovered from 3.04× (pre-backoff) to 3.02×.
> 2. **The per-fixture floors did not materialize.** The WS6–WS8
>    lanes landed and admit the frames they chartered (closure
>    cells, escaping callees, list shapes, `TO_BOOL`,
>    `LOAD_ATTR` residue), but admission ≠ win: the call-heavy
>    fixtures' time lives in the *callee bodies* and the generic
>    round-trips between them, not in the caller's loop scaffolding
>    the new lanes compile. `deltablue` (22.0×), `richards`
>    (11.8×), `pyaes` (17.2×), `list_ops` (16.9×) hold their
>    wave-10 envelope but not the chartered improvement ratios.
>    The new front line, measured: (a) `guards_hold` re-validation
>    cost per generic call — the epoch infrastructure WS9 deferred
>    is now the *named prerequisite* for call-heavy wins, since a
>    per-call snapshot walk is the tax the backoff can only avoid
>    by declining to compile; (b) callee-body coverage — the
>    fixtures' hot callees (dict/list method bodies, small
>    polymorphic methods) need to be *enterable* for the WS7 lane
>    to route calls natively instead of generically. Loop kernels
>    held ≤ 0.06×; `attr_access` measured its wave-11 lane win
>    (3.64× → 2.10× on the quietest run, high variance on this
>    machine). Rolls forward per the honest-miss protocol.

### Pillar III — experimental free-threading

#### WS10 — the design note

`docs/FREETHREADING.md`, the RFC 0066/0072/0075 debt, written from
the code rather than aspiration. Contents: (1) the heap model audit —
`sync::Rc = Arc` means refcounts are already atomic and
cross-thread-safe (the RFC 0024/0025 inheritance), so WeavePy needs
*no* biased-refcounting/immortalization tier, the single largest
piece of CPython's PEP 703 diff; (2) the inventory of what the GIL
actually guards today (dict/list/set internals, the type registry,
interned-string table, import lock, codegen caches, tier-1 inline
caches, tier-2 compiled-code publication); (3) the locking
discipline per class of state (per-object critical sections for
containers, sharded locks for runtime tables, epoch/seqlock reads
for caches); (4) the JIT posture (below); (5) the measured plan from
experimental mode to default.

#### WS11 — the `-X gil=0` runtime mode

One binary, no second ABI. Because WeavePy's object layout does not
change without the GIL (no `ob_refcnt` split), free-threading is a
**runtime mode**: `PYTHON_GIL=0` / `-X gil=0` starts the interpreter
with the GIL replaced by the WS10 locking discipline;
`sys._is_gil_enabled()` reports truthfully; default behavior is
unchanged.

- **Containers**: dict/list/set/bytearray mutation paths take a
  per-object critical section (a lock word in the object header's
  existing padding; uncontended fast path is one CAS). Read paths
  stay lock-free where the representation already tolerates it and
  seqlock-retry where it does not.
- **Runtime tables**: type registry, interned strings, and the
  import system move to sharded `RwLock`s; the import lock keeps
  CPython's per-module future semantics.
- **Tier-1/tier-2**: inline-cache writes become CAS-published;
  **tier-2 native entry is disabled under `gil=0` in this wave**
  (the interpreter runs tier-1 only), exactly CPython 3.13t's
  posture of disabling the specializing interpreter — re-enabling
  the JIT under free-threading is future work with its own RFC.
- **C extensions**: the 3.13t contract verbatim — importing an
  extension that does not declare `Py_mod_gil = Py_MOD_GIL_NOT_USED`
  re-enables the GIL for the process with a `RuntimeWarning`
  (override: `PYTHON_GIL=0` forces it off, on the user's head).
  `Py_mod_gil` slot parsing lands in the capi module-init path.
- **Out of scope, stated**: a `Py_GIL_DISABLED` ABI tag
  (`weavepy-3.13t` wheels), per-interpreter GILs (the RFC 0075
  own-GIL coercion stands), and JIT-under-free-threading.

#### WS12 — the free-threaded conformance lane

A new sweep flavor: `cargo run -p weavepy-conformance -- regrtest
--gil0 …` runs a scoped label set (the threading/concurrency family:
`test_threading`, `test_thread`, `test_concurrent_futures`,
`test_queue`, `test_asyncio` submodules, `test_importlib`
parallel-import legs, plus new bundled race-regression fixtures —
concurrent dict/list mutation, racing type creation, import storms)
under `-X gil=0`, graded against a new
`tests/regrtest/expectations-gil0.toml` measured baseline. The full
550-label sweep is *not* the gate for the experimental mode; the
scoped lane plus zero regressions in the default-mode sweep are.
A `threads=8` bench fixture pair (embarrassingly-parallel pure-Python
workload, GIL vs no-GIL) lands in `weavepy-bench` so the mode's
scaling claim is a measured number, not marketing.

**Measured outcome (2026-08, this wave).** The lane and the fixture
both landed measured. The `--gil0` lane grades **10/10 pass,
unexpected 0** across the bundled race fixtures
(threading primitives, cross-thread heap, eval breaker,
multiprocessing.dummy, `test_rfc0076_gil0.py`) plus the vendored
thread-family suites, stable across repeated runs
(`tests/regrtest/expectations-gil0.toml`). The scaling fixture
(`weavepy-bench scaling`, `fixtures/parallel_scaling.py`, 8 threads,
integer `+`/`*`/`%` kernel, macOS arm64): the default build reports
**0.90×** serial/parallel (the GIL serializes), `-X gil=0` reports
**3.26×** — the acceptance shape, measured. One contention find
rode along: the bitwise operators (`^`, `>>`, `&`) run ~5× slower
serially than `+`/`*`/`%` and serialize fully across threads under
`gil=0` (0.83–1.06× scaling at 2–8 threads) — a contended dispatch
path, not the GIL — recorded in `docs/FREETHREADING.md` as a
wave-12 burn target.

### Pillar IV — the 3.14 horizon

#### The version policy (adopted by this RFC)

WeavePy tracks **the latest stable CPython minor version** as its
single target, on a fixed trigger rather than a fixed date:

1. **Single-version trunk.** One `sys.version`, one grammar, one
   bytecode form, one vendored stdlib — like CPython itself. In-tree
   multi-version support is rejected permanently: a dual 3.13/3.14
   runtime would mean dual magics, dual verbatim stdlibs, and a
   forked conformance baseline, a standing tax with no drop-in
   audience. Users who need a frozen older surface get a
   **maintenance branch cut at each switch commit** (the CPython
   release-branch model), which receives fixes by cherry-pick only.
2. **The switch trigger.** Trunk switches to version N when (a)
   CPython N.1 — the first bugfix release — has shipped, and (b) the
   ecosystem matrix's packages publish cp31N wheels. Historically
   that is one to four months after the October release; it filters
   out the .0 churn while keeping WeavePy within months, not years,
   of current. By this rule the 3.14 switch is due *now* — and the
   same rule will call for 3.15 in early 2027.
3. **The switch protocol.** Every version wave is chartered by a
   measured gap sweep of the new `Lib/test` (the WS13 shape; RFC
   0036's original 3.13 protocol), lands as its own RFC, and
   re-baselines expectations from evidence — never from release
   notes.
4. **This wave measures; the next wave switches.** The 3.14 switch
   is the **committed next wave (RFC 0077)**, chartered by
   `docs/PY314-GAP.md`, not an optional follow-on. This wave stays
   on 3.13 because the four-front landing needs the zeroed 3.13
   scoreboard as its regression oracle: a version flip in the same
   commit would make a 3.14-delta failure indistinguishable from a
   pillar regression.

#### WS13 — the measured gap analysis

Vendor CPython 3.14.x's `Lib/test` under `vendor/cpython314/Lib/test`
(alongside, not replacing, the 3.13 tree) and run **one measured
sweep** of it under WeavePy. The output is `docs/PY314-GAP.md`: the
enumerated delta by category — grammar/tokenizer (t-strings, PEP 758,
deferred annotations), bytecode/magic (the 3.14 opcode delta as it
affects `cpython_code`/marshal/pyc), stdlib additions/removals
(`annotationlib`, `compression`, dead-battery stragglers), C-API
(PEP 741 done in-wave, the rest counted), and a first-failure-reason
census of every red label. This document is the upgrade wave's
charter, exactly as RFC 0036's first sweep was for 3.13. No 3.14
expectations baseline is committed — the sweep is a measurement, not
a gate.

#### WS14 — PEP 741 PyInitConfig

`crates/weavepy-capi/src/initconfig.rs` grows the 3.14 surface over
the RFC 0075 PEP 587 core: `PyInitConfig_Create/Free`,
`PyInitConfig_SetInt/SetStr/SetStrList`, `PyInitConfig_GetError`,
`PyInitConfig_HasOption`, `Py_InitializeFromInitConfig`, and the
runtime read side (`PyConfig_Get`, `PyConfig_GetInt`,
`PyConfig_Names`) — string-keyed lookups into the same config store,
so the two APIs stay coherent by construction. Embedders shipping
dual 3.13/3.14 support (the PyO3 tree already does) can compile
against WeavePy unchanged. The `_testembed` twin grows the PEP 741
legs so the surface is sweep-covered even though 3.13's `test_embed`
doesn't exercise it.

#### WS15 — additive 3.14 features

- **`compression.zstd` (PEP 784), ungated**: a new
  `crates/weavepy-vm/src/stdlib/zstd_mod.rs` over the `zstd` crate
  (`ZstdCompressor`/`ZstdDecompressor`/`ZstdDict`/`train_dict`,
  the `ZstdFile` Python layer, `compression.*` re-export package).
  Additive — no 3.13 test knows the package exists — and immediately
  useful (wheels and pips are moving to zstd). Graded by vendoring
  3.14's `test_zstd.py` as a bundled fixture.
- **PEP 750 t-strings + PEP 758 grammar, behind `-X lang=next`**:
  the tokenizer/parser accept `t"..."` prefixes and parenthesis-free
  multi-exception `except` only when the preview flag (or
  `WEAVEPY_LANG=next`) is set, keeping the 3.13 conformance sweep
  byte-identical by default. `string.templatelib` ships (importable
  regardless of the flag; constructing `Template` values requires
  the syntax). Bundled fixtures adapted from 3.14's `test_tstring`
  run under the flag in the regrtest harness's new `xflags` row key.
- **Deferred annotations (PEP 649/749) are explicitly *not* in
  scope**: they change 3.13-observable semantics and belong to the
  actual version-switch wave that `docs/PY314-GAP.md` charters.

### WS16 — re-measure and re-baseline

Per the RFC 0049 protocol: full default-mode regrtest sweep (`--mode
subprocess --jobs 8`) at `unexpected 0`; the new `--gil0` scoped lane
measured and committed; ecosystem lane re-run online and offline with
all touched selftest rows and the six new rows rewritten from
evidence; bench suite re-run and the macOS-aarch64 baseline
re-committed; new bundled regrtests for every engine fix (standing
policy); `cargo fmt` / `clippy -D warnings` / `cargo test
--workspace` green.

### Acceptance criteria

1. **Selftest zero**: numpy, scipy, Pillow, and lxml selftest rows
   grade `pass` with enumerated deselects (fallback per row: a
   `fail` whose census is fresh and names new root causes); the
   attrs selftest row is no longer `skip` (measured pass, or a
   scoped-profile run recorded in the row).
2. **Six new ecosystem rows** — polars, psycopg2, celery, uvicorn,
   alembic, torch — pass on macOS and Linux, offline from the wheel
   cache.
3. **Perf gate**: suite geomean ≤ 2.2× CPython on the re-committed
   baseline, per-fixture floors per WS9's gate paragraph, loop
   kernels ≤ 0.06×, no fixture outside its envelope. Honest-miss
   protocol: a miss lands with a fresh census and re-committed
   truthful baseline.
4. **Free-threading**: `weavepy -X gil=0` runs the WS12 scoped lane
   at its committed baseline; `sys._is_gil_enabled()` is truthful;
   a non-declaring C extension re-enables the GIL with a
   `RuntimeWarning`; the default-mode 550-label sweep is unaffected
   (`unexpected 0`); the parallel-scaling bench fixture reports >1×
   thread scaling under `gil=0` where the GIL build reports ~1×.
5. **`docs/FREETHREADING.md`** and **`docs/PY314-GAP.md`** exist and
   are grounded: the former's state inventory cites code, the
   latter's delta cites the measured 3.14 sweep (label counts +
   first-failure reasons).
6. **PEP 741** surface passes the `_testembed` twin's new legs and a
   layout/behavior unit suite; `compression.zstd` passes the
   vendored `test_zstd.py` fixture; t-strings/PEP 758 fixtures pass
   under `-X lang=next` and their syntax stays a `SyntaxError`
   without it (asserted).
7. Full default sweep `unexpected 0`; ecosystem `--check` green
   online and offline; `cargo fmt` / `clippy -D warnings` /
   `cargo test --workspace` green.

## Drawbacks

- **Four fronts in one landing is the largest wave yet.** Each
  pillar alone matches a historical RFC's scope. Mitigation: the
  pillars are dependency-ordered, not entangled — Pillar II is
  JIT-crate-local, Pillar III is gated behind a flag that defaults
  off, Pillar IV is additive or preview-gated — so a slip in one
  does not corrupt the others' measurements; each pillar has an
  independent honest-fallback shape.
- **Free-threading touches every container's mutation path.** Even
  with the mode off by default, the lock-word plumbing and the
  CAS-published caches run in default mode too. Mitigation: the
  uncontended fast path is designed to be one relaxed check in
  default mode; the bench gate (criterion 3) will catch a
  default-mode regression because it runs in-wave.
- **The torch row is a heavyweight hostage.** A 200 MB wheel, a long
  import, and a package famous for exercising dark C-API corners.
  Mitigation: the probe is scoped (no `torch.compile`, CPU only),
  the budget is its own manifest entry, and — like grpcio in RFC
  0072 — a measured `fail` row with a named root cause is an
  acceptable landing for a *new* row (not for the WS1–WS4 burns).
- **`-X lang=next` is a fork of the grammar.** Preview gates rot if
  the upgrade wave slips. Mitigation: the gate's implementation is
  the 3.14 parser work done early, not throwaway — the upgrade wave
  flips the default and deletes the flag.
- **The 2.2× gate has been missed once already.** Wave 10 held flat
  against the same number. Difference this time: wave 10 built
  lanes and *discovered* the front line; this wave starts from that
  measured census with the rejection counters already landed. The
  honest-miss protocol stands regardless.
- **Wheel-cache and CI-time growth**: six new rows plus torch add
  ~300 MB per platform and real minutes. Same posture as every
  ecosystem wave: pins in the manifest, cache keyed on it, budgets
  per row.

## Alternatives

- **Four separate waves (the historical cadence)**: rejected per the
  explicit one-landing request — and the coupling is real: WS4/WS1's
  budget rows need Pillar II's interpreter; Pillar III's design note
  needs WS11's implementation to stay honest; landing them together
  means one sweep validates all four instead of four waves of
  re-baselining.
- **Free-threading as a design note only (no implementation)**:
  rejected — the note has been "owed" for ten RFCs precisely because
  prose without a mode to measure keeps slipping; the experimental
  mode is what makes the note's claims falsifiable.
- **A separate `weavepy-3.13t` ABI build for free-threading**:
  rejected — WeavePy's layout doesn't change without the GIL, so a
  second ABI would be ceremony; the runtime mode delivers the same
  experiment with zero distribution surface. The ABI tag question
  returns if/when free-threaded *wheels* matter.
- **Jumping the target to 3.14 outright**: rejected for this wave —
  the 3.13 surface is a zero-scoreboard asset and the selftest burn
  is against packages' current cp313 wheels; the measured gap
  analysis is how the jump gets chartered without re-opening the
  scoreboard blind.
- **Skipping torch (too heavy) in favor of more mid-size rows**:
  rejected — the matrix's marginal information now comes from
  extremes: polars (Rust/abi3) and torch (the heaviest C++/C-API
  consumer) bound the space; another flask-shaped row does not.
- **Sharding numpy's budget-bound modules now instead of waiting for
  Pillar II**: kept as the fallback (the RFC 0075 WS8 shard
  mechanism is landed); the preferred shape is fitting the budget on
  the faster interpreter because it also fixes attrs.

## Prior art

- **CPython 3.13t/3.14t (PEP 703, PEP 779)**: the extension
  `Py_mod_gil` contract, `PYTHON_GIL`/`-X gil` UX, and
  disabling-the-specializer posture are adopted verbatim; the
  biased-refcounting/immortalization machinery is *not* needed here
  (the Arc heap predates this RFC by fifty waves), which is the
  design note's central claim.
- **PyPy's GIL work and Jython/IronPython's no-GIL history**: fine-
  grained container locking is viable but the win is workload-
  dependent — hence WS12's measured scaling fixture instead of an
  assumed speedup.
- **The nogil-3.9/3.12 forks (Sam Gross)**: demonstrated that cache
  publication and dict internals dominate the correctness risk —
  mirrored in WS11's container-first lock inventory.
- **PyO3's dual 3.13/3.14 + freethreaded support matrix**: the
  consumer polars rides; its build probes for exactly the PEP 741
  and `Py_mod_gil` surfaces this wave lands.
- **RFC 0036's first measured 3.13 sweep**: the shape WS13 copies
  for 3.14 — measure first, charter from the census, commit no
  baseline until the target is declared.
- **RFC 0074's honest-miss landing**: the perf-gate protocol Pillar
  II inherits (a miss re-commits truthful numbers and a fresh
  census, never an aspirational row).

## Unresolved questions

- **Does torch's cp313 wheel import under the 725-symbol ABI, or
  does it surface a new NULL-stub tranche?** torch links more of the
  C-API than any row to date; the first import is the measurement.
  The row's acceptance allows a measured fail-with-reason landing.
- **Is ≤ 2.2× reachable from 2.85× on this charter?** The census
  says the gated fixtures reject on exactly the WS6–WS8 shapes, but
  wave 10 teaches that burned shapes reveal the stratum behind them.
  The honest-miss protocol is the answer either way.
- **How far does the uncontended-lock fast path stay free in default
  mode?** The lock word rides existing header padding and the
  default-mode path is one relaxed load — but `dict` is hot enough
  that "one relaxed load" must be verified by the bench gate, not
  asserted.
- **attrs under hypothesis**: if the wave-11 interpreter is 1.5×
  faster on the attrs shapes, the suite may still exceed 2400 s.
  The scoped-profile fallback is documented in WS4; whether upstream
  considers that a faithful run is a judgment recorded in the row.
- **celery's worker-in-a-thread probe stability**: billiard's
  process shims under `--pool=solo` should stay in-process, but
  celery's shutdown discipline is historically racy; the probe may
  need a generous drain timeout, recorded in the manifest.
- **How much of 3.14's `test_free_threading/` directory is
  meaningful under WS11's mode** (it assumes CPython's tstate
  internals in places); the WS12 lane cherry-picks the portable
  labels and the gap analysis enumerates the rest.

## Future work

- **The 3.14 switch wave (RFC 0077 — committed, not optional, per
  the version policy above)**: flip the target per
  `docs/PY314-GAP.md` — deferred annotations (PEP 649/749), the
  bytecode/magic delta, `annotationlib`, `python314.dll` /
  `libpython3.14` / cp314-tag identity, defaulting t-strings/PEP 758
  and deleting `-X lang=next`, cutting the `weavepy-3.13`
  maintenance branch, and the dual-baseline retirement.
- **JIT under free-threading**: re-enabling tier-2 native entry with
  epoch-guarded caches under `gil=0` — its own RFC, chartered by the
  WS12 lane's measurements.
- **Free-threaded wheels** (`weavepy-3.13t`-tagged) if and when the
  ecosystem's `cp31xt` wheel coverage makes them consumable.
- **mod_wsgi / PyO3-embed ecosystem rows** (RFC 0075 debt, still
  owed): the polars row covers PyO3-as-extension; PyO3-as-embedder
  remains.
- **Full-suite scipy/scikit-learn selftests** on the wave-11
  interpreter if the WS2 re-measure shows the budget is now sane.
- **Package-manager distribution** (brew formula, pyenv plugin,
  release signing) — the RFC 0062 non-goals, unblocked but unclaimed.
