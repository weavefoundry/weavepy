# RFC 0057: The long tail — object-model fidelity, compiler introspection, import machinery, `_decimal`, and pickle protocol 5

- **Status**: Accepted
- **Authors**: WeavePy authors
- **Created**: 2026-08-03
- **Tracking issue**: TBD
- **Builds on**: RFC 0049 (measured whole-suite baseline protocol),
  RFC 0051/0052 (core-language + compiler front-end fidelity lineage),
  RFC 0053 (source-truth stdlib; verbatim files step on VM gaps),
  RFC 0031 (observability hooks this wave makes event-exact),
  RFC 0033 (code-object surface this wave completes), RFC 0056
  (ecosystem wave 2; its Results section names this wave's residuals).

## Summary

RFC 0056 left the sweep at **542 total — pass 418 / fail 113 / skip 8 /
timeout 3 — unexpected 0**. The ecosystem lane is 27/27 green, Django
serves requests, and real binary wheels import. What remains is not a
missing subsystem — it is *the long tail*: 113 measured red rows whose
first-failure reasons cluster into a handful of engine-fidelity themes,
plus two principled skips (`test_decimal`, `test_pickle`) that every
"is it really a drop-in?" audit reaches for first.

This wave is a burn-down, not a bring-up. The clusters, from the
measured reasons in `tests/regrtest/expectations.toml`:

1. **Object model & descriptors (~19 rows).** Slot descriptors that
   reject unbound access (`test_abstract_numbers`: "slot descriptor
   requires an instance"), attribute stores through non-string keys
   (`test_baseexception`), missing `__float__` delegation on int
   subclass paths (`test_cmath`), `frame.f_lineno` absent
   (`test_frame`), method objects rejecting attribute probes with the
   wrong exception (`test_funcattrs`), builtin `__init__` kwargs
   (`test_property`), exception `args` stored as a pseudo-slot leaking
   into `__dict__` (`test_xmlrpc`, RFC 0056 residual), ExceptionGroup
   refusing to nest `BaseException`s (`test_exception_group`), plus
   `structseq`, `metaclass`, `dynamicclassattribute`, `userlist`,
   `reprlib`, `context`, OrderedDict/defaultdict residuals, and
   dict watchers/versioning.
2. **Compiler / AST / bytecode introspection (~15 rows).** `test_ast`
   alone carries 169 failures / 80 errors (node construction, position
   fidelity, full AST validation); `compile()` lacks `PyCF_*` flags and
   AST input; `code` objects lack `co_lnotab`; `dis`/`peepholer`/
   `test__opcode`/`compiler_{assemble,codegen}` assert CPython's exact
   optimizer output; the parser rejects PEP 646 starred annotations
   (`def f(*args: *tuple[int, ...])` — first failure of both
   `test_inspect` and `test_pep646_syntax`); `unparse`, `patma`,
   `positional_only_arg`, `source_encoding`, `type_comments` carry
   front-end residuals.
3. **Import machinery & module metadata (~13 rows).** Frozen modules
   lack `__spec__` (`test_frozen`), `importlib.machinery` lacks
   `AppleFrameworkLoader` (first failure of `test_import` *and*
   `test_types` on macOS), `module.__annotations__` autoviv is missing
   (`test_module`), `_imp` lacks the frozen-table introspection the
   ctypes residual needs (`test_ctypes.test_frozentable`, named in RFC
   0056's results), plus `FileFinder` semantics (`test_importlib`),
   `modulefinder`, `test_pkg`, `test___all__`, `test_site`.
4. **Scope & unpacking semantics (~7 rows).** Three comprehension
   suites die on the *same* signature (`TypeError: unsupported operand
   type(s) for -: 'NoneType' and 'int'` — one root cause, three
   flips), `named_expressions` hits a comprehension-scope NameError,
   `unpack_ex` fails starred-target unpacking with generators,
   `yield_from` and `iterlen` carry generator-protocol residuals.
5. **Builtins & numerics edges (~8 rows).** `int()` literal error
   fidelity (`test_builtin`), `pow` edge cases, `print` flush
   accounting, `range` attribute-error taxonomy + i64-overflow ranges +
   `5.0 in range(10)`, `long` formatting, `sort` stability probes,
   `bigmem` guards, `strtod` round-tripping.
6. **Observability event-exactness (~7 rows).** `settrace` is down
   from 159F/16E to 58F/0E — the residual is line-event granularity;
   `setprofile` event ordering; `tracemalloc.Traceback` /
   `Snapshot` surface; PEP 669 residuals (`test_monitoring`);
   `faulthandler` stack-header format; `test_trace`, `test_atexit`,
   `test_audit`, and `_lsprof` calibration (`test_cprofile`).
7. **`_decimal` (1 principled skip + the module every auditor
   checks).** The pure-Python `decimal` is complete but `test_decimal`
   probes the C accelerator's contexts, thread-local state, exact
   exception taxonomy, and IEEE 754 payload behavior. This is the
   last "wave-sized artifact" named in three consecutive RFCs'
   future-work sections (0041, 0054, 0056).
8. **Pickle protocol 5 (1 principled skip + 3 red rows).**
   `PickleBuffer`, out-of-band buffers, and a native `_pickle` flip
   `test_pickle`, `test_picklebuffer`, `test_pickletools`, and the
   `test_pyclbr` residual (`Pickler.__module__ == '_pickle'`), and
   unlock `multiprocessing` shared-memory patterns.
9. **The three timeouts.** `test_deque`, `test_mmap`, `test_weakref`
   are throughput problems (weakref measured at ~125s against a 60s
   budget), not hangs — container fast-paths and weakref-callback
   overhead, plus honest per-row budgets where the suite is
   legitimately slow under a debug-profile interpreter.

As with every wave since RFC 0036, the deliverable is measured: two
cross-checked full sweeps, every touched row rewritten from evidence,
reds allowed with reasons mandatory, `unexpected 0`.

## Motivation

1. **The README's claim is now gated by exactly this tail.** After
   RFC 0056, no *subsystem* is missing: networking, asyncio, TLS,
   sqlite3, XML, the binary ABI, and the packaging story all exist and
   are measured. A skeptical reader running the sweep sees 113 reds
   whose reasons are "engine fidelity" — precisely the category the
   project's first goal ("dark corners included") promises to close.
2. **The clusters are known, so the work de-risks itself.** Unlike a
   bring-up wave, every row here has a measured first-failure string.
   The top three clusters (object model, compiler introspection,
   import metadata) account for ~47 rows and share substrates, so
   fixes compound: `co_lnotab` alone appears in the first-failure
   chain of four rows; the PEP 646 parser gap gates two.
3. **`test_decimal` and `test_pickle` are the audit-trail skips.**
   They are the only remaining rows where the answer to "why is this
   red?" is "we chose not to build it yet" rather than a measured
   residual. `decimal` is load-bearing for financial code and
   `fractions`/`statistics` interop; pickle 5 is load-bearing for
   dataframes-over-multiprocessing. Both have exact, well-documented
   specs (libmpdec semantics; PEP 574) — ideal one-wave artifacts.
4. **Timeout rows poison sweep hygiene.** A timeout is the one status
   that can mask a regression (a new hang grades the same as "slow").
   Retiring all three restores the invariant that every non-pass row
   has a *semantic* reason.
5. **Cost of inaction.** Every future wave (ecosystem wave 3, Windows,
   free-threading) builds on the object model and compiler surfaces
   this wave hardens. Deferring the tail again means every subsequent
   RFC keeps paying the "measured residual" tax on rows whose root
   causes are already understood.

## CPython reference

- `Objects/typeobject.c` — slot descriptor binding (`slot_tp_*`,
  `wrap_descr_get`), `tp_getset` unbound-access rules,
  `type_new` metaclass negotiation, `__set_name__` ordering.
- `Objects/frameobject.c` — `f_lineno` (computed from `co_linetable`
  + `f_lasti`, *writable* under trace), `f_trace_lines`/
  `f_trace_opcodes`, `frame_setlineno` jump validation.
- `Objects/exceptions.c` — `args` as a real slot (`BaseException`
  struct member, never in `__dict__`), `__notes__`,
  ExceptionGroup nesting rules (`BaseExceptionGroup.__new__` choosing
  the subclass by payload), `PyErr_SetObject` normalization.
- `Objects/funcobject.c` / `classobject.c` — function attribute
  taxonomy (`known_attr` probes raise `AttributeError`, methods proxy
  reads to `__func__` but reject writes with `AttributeError`).
- `Python/compile.c`, `Python/flowgraph.c` — the exact peephole
  pipeline (`optimize_basic_block`, constant folding order,
  `LOAD_FAST` superinstructions), `co_lnotab` back-compat synthesis
  from `co_linetable`, `PyCF_ONLY_AST` / `PyCF_ALLOW_TOP_LEVEL_AWAIT`
  / `PyCF_TYPE_COMMENTS` / optimize levels in `compile()`.
- `Python/ast.c` + `Parser/` — AST validation (`_PyAST_Validate`),
  node constructors with position defaulting, PEP 646
  `Starred` in annotation grammar.
- `Lib/importlib/_bootstrap.py` — `FrozenImporter.find_spec` (real
  `ModuleSpec` with `origin='frozen'`), `module.__annotations__`
  via the module `__getattr__` protocol, `AppleFrameworkLoader`
  (3.13 iOS/macOS framework loader — must *exist* even when unused).
- `Python/symtable.c` + PEP 709 notes — comprehension scoping:
  inlined comprehensions still isolate the iteration variable; the
  measured `NoneType - int` signature is our compiler leaking the
  comprehension's `.0` slot lifetime into the enclosing frame's
  fast-locals under nested/class-body comprehensions.
- `Modules/_decimal/` + `libmpdec/` — the accelerator: `Decimal`,
  `Context` (thread-local via `contextvars` in 3.13), signals as
  class hierarchy (`DecimalException` → `InvalidOperation` /
  `DivisionByZero` / `Inexact` / `Rounded` / `Subnormal` /
  `Overflow` / `Underflow` / `Clamped` + `FloatOperation`),
  `localcontext`, exact `quantize`/`__round__`/format-spec behavior,
  `as_integer_ratio`, IEEE contexts (`IEEEContext`, `MAX_PREC`).
- PEP 574 (`Modules/_pickle.c`, `Lib/pickle.py`,
  `Lib/pickletools.py`) — `PickleBuffer` (buffer-exporting,
  `raw()`/`release()`), protocol 5 opcodes (`NEXT_BUFFER`,
  `READONLY_BUFFER`, `BYTEARRAY8`), `buffer_callback=` /
  `buffers=` round-trip, `Pickler.__module__ == '_pickle'`.
- `Modules/_collectionsmodule.c` (deque block layout),
  `Modules/mmapmodule.c`, `Objects/weakrefobject.c` (callback
  fast path, `WeakMethod`) — the throughput references for the
  timeout rows.
- Acceptance tests: every row named in the Summary clusters, plus
  `test_decimal.py` and `test_pickle.py` graduating from skip.

## Detailed design

### WS1 — object-model fidelity burn

Measured-first over the ~19 rows. The known root causes, each landing
a bundled regrtest when it is engine behavior:

- **Slot/getset descriptor binding**: unbound access through
  `SomeType.__float__`-style getset and wrapper descriptors must
  return an unbound descriptor usable via explicit `__get__`, and the
  error text for instance-required slots must match
  `descrobject.c` (`"descriptor '<name>' for '<type>' objects doesn't
  apply to a '<other>' object"` vs our current generic "slot
  descriptor requires an instance").
- **Exception `args` as a real slot** shared with `__notes__`: move
  `args` out of the instance dict into the native exception layout,
  make `__dict__` truthful (fixes `test_xmlrpc`'s `Fault.__dict__`
  and `test_baseexception`'s non-string attribute probes, which
  currently die in our dict-backed store before `__setattr__`
  raises the right `TypeError`).
- **`frame.f_lineno`** computed from the RFC 0033 linetable +
  `f_lasti`, writable only under an active trace function with
  CPython's jump-validity rules (also unblocks part of the WS6
  settrace residual — `test_sys_settrace`'s jump tests).
- **Function/method attribute taxonomy**: arbitrary attribute reads
  on `method` objects proxy to `__func__` then raise
  `AttributeError` (not `TypeError`); function `__dict__` semantics
  per `funcobject.c`.
- **`int.__float__` / numeric delegation** on the `numbers` ABC
  paths (`test_abstract_numbers`, `test_cmath`).
- **Builtin `__init__` keyword acceptance** where CPython's clinic
  signatures take kwargs (`property(fget=…)` et al.) — finish the
  RFC 0049 argument-clinic arity pass over the constructor surface.
- **ExceptionGroup nesting** (`BaseExceptionGroup` containing
  `BaseException`s selects the base class; `ExceptionGroup` rejects
  them at construction), `split()`/`subgroup()` identity rules.
- **`structseq`** — real `n_fields`/`n_sequence_fields`/unnamed-field
  semantics and pickling for `os.stat_result`-family types.
- **The rest measured in place**: `metaclass` (classdict exec order
  + `__mro_entries__` edges), `DynamicClassAttribute`, `UserList`
  slicing returns, `reprlib.recursive_repr`, `contextvars.Context`
  run/copy semantics, OrderedDict/defaultdict C-parity residuals,
  dict versioning/watchers (`test_dict_version` wants the
  `ma_version_tag` behavior `_testcapi` exposes — stub the observer,
  keep the tag maintenance real since RFC 0048 already maintains it
  for guards).

### WS2 — compiler, AST, and bytecode introspection

The RFC 0052 front-end lineage, finished:

- **`ast` node fidelity**: constructors accept/default positions per
  `_PyAST_Validate`, `_fields`/`_attributes` exact, missing-field
  errors match, `ast.parse(feature_version=…)` honored, full
  validation errors (the 169F/80E burn is mostly mechanical once
  constructor defaulting and validation land — both are table-driven
  from the ASDL we already vendor).
- **`compile()` completion**: `PyCF_ONLY_AST` (returns our real AST
  objects), AST-input compilation (`compile(tree, …)` walks the same
  lowering path as source), `PyCF_ALLOW_TOP_LEVEL_AWAIT`,
  `PyCF_TYPE_COMMENTS` (with `# type:` tokens surfaced —
  `test_type_comments` rides this), `optimize=` levels with CPython's
  exact docstring/assert stripping.
- **PEP 646 grammar**: `Starred` in annotation and subscript
  positions (`def f(*args: *Ts)`, `tuple[int, *Ts]`) — unblocks
  `test_inspect` + `test_pep646_syntax` at the parser layer.
- **`co_lnotab`** synthesized lazily from `co_linetable` exactly as
  CPython's back-compat shim does (flips the `test_code` first
  failure; `test_dis`/`test_peepholer` chains re-measure behind it).
- **Peephole parity where tests assert it**: constant folding
  (including frozenset/tuple folding and `not`/`is not` fusions),
  `LOAD_FAST_LOAD_FAST`-family superinstructions, dead-code
  elimination shapes that `test_peepholer` / `test_dis` /
  `test_compiler_{codegen,assemble}` assert literally. Where our
  emission is *better* but different, we adopt CPython's shape —
  the suites are the spec, per project goal 1.
- **`ast.unparse`** residuals (precedence/parenthesization cluster),
  `test__opcode` (the `_opcode` module's `stack_effect` /
  `get_specialization_stats` surface over our real tables),
  `source_encoding` (PEP 263 cookie edge cases), `patma` residuals
  (measured; the RFC 0009 engine is complete so these are expected
  to be error-message/AST-position fidelity).

### WS3 — import machinery and module metadata

- **Frozen `ModuleSpec`s**: `FrozenImporter` produces real specs
  (`origin='frozen'`, `__spec__` set on `__phello__` and friends),
  `_imp.is_frozen_package` / `_imp._frozen_module_names` / frozen
  C-table introspection lands (also closes RFC 0056's enumerated
  `test_ctypes.test_frozentable` residual).
- **`AppleFrameworkLoader`** exists in `importlib.machinery` with
  CPython 3.13's class surface (it only activates on framework
  builds; existence is what `test_import`/`test_types` assert).
- **`module.__annotations__`** autovivification through the module
  `__getattr__`/descriptor protocol, plus `__dir__` truthfulness.
- **`FileFinder`** path-hook semantics (`path_importer_cache`
  invalidation, `find_spec` on stale dirs), namespace-package
  `__path__` re-computation — the `test_importlib` first-failure
  chain, burned measured-first.
- **Re-measure behind those**: `test_pkg`, `test___all__` (walks
  every stdlib module's `__all__` — expected to surface small
  export-list gaps we fix inline), `test_site` (user-site dirs +
  `sitecustomize` hooks), `test_modulefinder` (bytecode-scanning
  over our real code objects — expected free after WS2's
  `co_lnotab`).

### WS4 — scope and unpacking semantics

- **The comprehension bug**: our compiler assigns the comprehension
  iterator to a fast-local slot whose lifetime collides with the
  enclosing frame under PEP 709-style inlining when the comprehension
  appears in a class body or nested comprehension — the measured
  `NoneType - int` is a clobbered enclosing local read back as
  `None`. Fix the slot isolation (CPython isolates `.0` and the
  iteration variables even when inlining); three rows flip on one
  fix, `named_expressions`' comprehension-scope `NameError` rides
  the same symtable pass.
- **Starred-target unpacking with generators** (`a, *b = gen()`):
  our current path materializes through a list op that mishandles
  the arity error case; port `unpack_iterable`'s exact
  before/after-star accounting and error messages.
- **`yield from` / `iterlen` residuals**: measured; expected to be
  `send`/`throw` delegation edges and `__length_hint__` fidelity on
  the builtin iterator family.

### WS5 — builtins and numerics edges

Small, enumerable, each with a bundled regrtest:

- `int()` invalid-literal messages quote the *original* string with
  CPython's truncation rules; `int(x, base)` non-string base errors.
- `pow()` three-arg edge cases (negative exponent with modulus,
  `0 ** 0 % 1`) and float/complex promotion taxonomy.
- `print(flush=True)` write/flush call accounting on file-likes.
- `range`: attribute errors are `AttributeError` (not `TypeError`),
  full-i64 (and beyond, via bigint) start/stop/step, float
  membership uses `__eq__` scan semantics (`5.0 in range(10)` is
  `True`).
- `long` (`int`) formatting residuals, `sort` stability/key probes,
  `bigmem` decorator guards (they should *skip* cleanly on our
  memory accounting, not error), `strtod` exact round-trip
  (`float(repr(f)) == f` across the suite's corpus — expected
  mostly green already; burn the residual).
- `str`/`userstring` residual F/E clusters re-measured after the
  above (their reasons overlap the clinic/error-message work).

### WS6 — observability event-exactness

- **`settrace` line events**: emit per-line events exactly where
  CPython's `co_linetable` boundaries fall (no duplicate events on
  backward jumps unless the line changes; `f_trace_lines=False`
  suppression; opcode events behind `f_trace_opcodes`), and support
  the `f_lineno` jump assignments WS1 lands. Target: the 58
  residual failures reach zero or an enumerated handful.
- **`setprofile`**: c_call/c_return/c_exception events on builtin
  boundaries with CPython's ordering relative to Python-level
  call/return.
- **`tracemalloc`**: `Traceback`/`Frame`/`Statistic`/`Snapshot`
  objects with `statistics()`/`compare_to()`, `get_object_traceback`.
- **`monitoring`** (PEP 669) residuals: `DISABLE` semantics,
  per-tool event masks, `events.NO_EVENTS` edges.
- **`faulthandler`** dump format byte-parity (`Current thread 0x…`
  header, `File "…", line N in <name>` frames).
- **`atexit`** callback error reporting shape; **`sys.audit`**
  residual hook coverage (the missed events enumerated by
  `test_audit`); **`_lsprof`** calibration + `Profiler` stats
  shape for `test_cprofile`; `test_trace` (the stdlib tracer)
  re-measured behind settrace exactness.

### WS7 — `_decimal`

A native accelerator with libmpdec *semantics* (not a libmpdec
vendoring — see Alternatives):

- **`stdlib/decimal_native/`** family: `Decimal` as a native heap
  type over a sign/coefficient(bigint)/exponent triple, `Context`
  with 3.13's thread-local-by-default state (`getcontext`/
  `setcontext`/`localcontext`, `contextvars`-backed), the nine-signal
  exception hierarchy with flag/trap semantics, and the full
  operation table (arithmetic, `quantize`, `compare_*` family,
  `logb`/`scaleb`, `ln`/`log10`/`exp`/`sqrt`/`power` with correct
  rounding via the same digit-schoolbook algorithms the spec
  defines, `to_integral_*`, `normalize`, `canonical`,
  `as_integer_ratio`, `as_tuple`, `__format__` per the
  format-spec mini-language, `__round__`, hash equal to
  `hash(Fraction(d))` per the numeric-hash invariant).
- **Correctness source**: the General Decimal Arithmetic
  specification testcases that `test_decimal` already carries
  (`decimaltestdata/*.decTest`, vendored with CPython's suite) —
  the suite runs both implementations; ours must match the
  pure-Python one everywhere and the C one on
  implementation-detail probes the suite marks `@requires_cdecimal`.
- **Adoption**: verbatim `Lib/decimal.py` (`from _decimal import *`
  with the pure `_pydecimal` fallback kept), `test_decimal` flips
  from principled skip to a measured row.

### WS8 — pickle protocol 5 and a native `_pickle`

- **`PickleBuffer`** as a native buffer-exporting type
  (`raw()`, `release()`, PEP 3118 integration with the RFC 0028
  buffer machinery).
- **Protocol 5 opcodes** in both directions (`NEXT_BUFFER`,
  `READONLY_BUFFER`, `BYTEARRAY8`), `Pickler(buffer_callback=)` /
  `Unpickler(buffers=)` round-trip.
- **A native `_pickle`** module (the accelerator identity matters:
  `test_pyclbr` asserts `Pickler.__module__ == '_pickle'`), with
  the verbatim `Lib/pickle.py` dispatching to it and the
  memo/framing behavior `test_pickletools` disassembles.
- `test_pickle` flips from skip; `test_picklebuffer`,
  `test_pickletools` re-measured; `multiprocessing` reduction
  re-measured behind it (out-of-band buffers are its shared-memory
  fast path).

**Landed (measured)**: `test_pickle` 980 tests / 0 failures / 62
skips (the same principled skips as CPython), `test_picklebuffer`
and `test_pickletools` green. The accelerator lane is a frozen
`_pickle` re-export module over the pure engine that reproduces the
C module's error discipline (truncation/underflow →
`UnpicklingError`, `save_reduce` argument validation, reentrancy
guards, memo validation) rather than a Rust rewrite; identity probes
(`pickle.Pickler is pickle._Pickler`) still distinguish the lanes.
Load-bearing VM work that landed with it: `DICT_MERGE` kwargs
strictness, lazy `map`/`filter` (own-type reduce), interned instance
attribute keys, `PickleBuffer` exporter delegation
(`memoryview(PickleBuffer(b)).obj is b`), a per-module import lock
(bpo-34572 unpickle module race), proto-0/1 bytes pickles emitting
`_codecs encode` byte-for-byte, and pickle-5 zero-copy support in
the pure-numpy shim.

### WS9 — retire the timeouts

- **`test_deque`**: block-based storage (the current ring buffer
  degrades on the suite's rotate/maxlen stress patterns) or targeted
  fast-paths — measured by profile, fixed to fit the 60s budget
  with headroom.
- **`test_mmap`**: the suite's large-file resize/slice patterns hit
  our byte-at-a-time fallback; vectorize the slice paths.
- **`test_weakref`**: ~125s measured — callback dispatch allocates
  per-deref; cache the callback vector and fast-path dead-ref
  checks. The residual object-model gaps on the row (proxy
  richcompare, `WeakMethod` rebinding) are burned in WS1 style.
- Where a suite is legitimately >60s under a debug-profile
  interpreter *after* the fixes, the row gets an honest
  `timeout_seconds` override per the RFC 0051 precedent — but the
  status must be `pass`.

**Result (landed):** no `timeout` rows remain. `test_mmap` turned out
to be a correctness gap, not a perf gap: the shim's byte paths
panicked (`read` past a shrunk mapping) and the surface was a
fraction of `mmapmodule.c`. Rebuilt on raw `mmap(2)`: real
`flags`/`prot`/`offset`/`trackfd` constructor semantics with
CPython's validation order (empty-file / offset-vs-size /
length-vs-size `ValueError`s, access-vs-flags conflict, fd dup via
`F_DUPFD_CLOEXEC`), all construction in `__new__` so subclasses can
delegate `mmap.mmap.__new__(cls, -1, …)`, extended-slice
subscripting, `find`/`rfind` with slice-notation bounds defaulting
`start` to the current pos, `move`/`madvise`/`flush` bounds
discipline, `seek` returning the new position, `size()` by fstat of
the dup'ed fd (EBADF for anonymous / `trackfd=False`, as CPython),
`resize` gated by export/trackfd/access checks (mremap on Linux,
CPython's own `SystemError` elsewhere), the `closed` property,
CPython's `__repr__` format, weakref support, and `_sre` matching
directly over the mapping (`re.search(b'…', m)`). 42 tests OK in ~4s
(9 skips: Windows-only + `@cpython_only` + the no-mremap resize skip
CPython itself takes on macOS); `test_deque` and `test_weakref` were
retired earlier in this workstream.

### WS10 — stdlib residual burn

Fragmented rows, burned measured-first with the standing "adopt
verbatim + fix the VM gap it steps on" policy: `email` (policy.utf8 +
`iter_attachments`), `pathlib` (walk/glob cluster), `logging`
(post-handler residuals), `random` (Mersenne state save/load +
`SystemRandom`), `pydoc` (`KeyError: '__doc__'`), `configparser`,
`ipaddress`, `tomllib`, `secrets`, `shlex`, `rlcompleter`,
`code_module`, `zoneinfo` (the RFC 0056-enumerated weak-cache trio),
`strptime`/`time` (timezone residuals), `hash`/`hashlib`
(siphash13 vectors + blake2/sha3 constructor surface), `marshal`
(the 21F/12E residual), `re` (Unicode-property residuals),
`resource`, `sys`, `threading_local` (native `_thread._local`),
`ntpath.ALLOW_MISSING`, `test_support`/`test_regrtest` (harness
self-tests), `urllib2_localnet`, `memoryio`/`memoryview`/`array`/
`buffer` (`_from_flags` + PEP 3118 tail), `fileio`/`fileinput`/
`file_eintr` (`_blksize` + EINTR retry), `fork1` (exit-code
propagation), `capi`/`type_cache`/`fileutils` (the `_testcapi` /
`_testinternalcapi` stub tail), `optimizer` (`_testinternalcapi`
uop probes — stub honestly or skip principled). Rows that stay red
get re-measured reasons; rows whose remaining reason is "CPython
implementation detail with no public contract" get documented
principled skips, used sparingly.

### WS11 — re-measure and re-baseline

Per the RFC 0049 protocol: two full sweeps
(`regrtest --all-cpython --mode subprocess --jobs 8`) cross-checked;
every touched row rewritten from evidence; the ecosystem offline lane
re-verified (27/27 must hold); new bundled regrtests for every engine
fix (slot-descriptor errors, exception-slot storage, `f_lineno`
jumps, comprehension isolation, PEP 646 grammar, `co_lnotab`,
frozen specs, decimal signal matrix, pickle-5 round-trip,
deque/weakref perf canaries); README status paragraph and
`docs/CONFORMANCE.md` updated with the new baseline.

**Sweep regression grading (landed):** the first full sweep surfaced
three engine bugs fixed during re-baseline. (1) `staticmethod`-wrapped
C functions (`object.__new__`, `str.maketrans`) registered in the
descriptor registry were reclassified as `method_descriptor`; a new
`DescrKind::StaticBuiltin` keeps their `__qualname__`/`__objclass__`
metadata while their type stays `builtin_function_or_method` as in
CPython (inspect's `_NonUserDefinedCallables` gate —
`test_warnings` deprecated-class signatures). (2) VM-internal lazy
machinery loads (`module.__repr__` reaching for
`importlib._bootstrap`) executed their import statements through a
user-patched `builtins.__import__`, letting testmock's
`patch('builtins.__import__')` clobber `sys.modules['sys']`;
`IMPORT_NAME` now bypasses the hook inside `import_path_internal`,
matching CPython where the bootstrap chain is frozen and initialized
before user code runs (`test_unittest` discovery/buffering fallout).
(3) `faulthandler.register(chain=True)` omitted `SA_NODEFER`, so the
chained `raise()` stayed pending and redelivered to the re-installed
handler in an unbounded signal loop (`test_faulthandler`
`test_register_chain` hang → suite timeout).

### Acceptance criteria

1. The comprehension-scope root cause is fixed;
   `test_listcomps`/`test_dictcomps`/`test_setcomps` and
   `test_named_expressions` flip.
2. `frame.f_lineno` (read + traced write), exception `args`-as-slot,
   and the slot-descriptor error taxonomy land with bundled
   regrtests; ≥ 12 of the ~19 WS1 rows flip.
3. `compile()` accepts AST input and the `PyCF_*` flags;
   `co_lnotab` lands; PEP 646 annotations parse; `test_ast`'s
   internal failure count drops below 25 (from 169F/80E) with the
   row flipped or carrying an enumerated residual; ≥ 8 of the ~15
   WS2 rows flip.
4. Frozen modules carry real specs, `AppleFrameworkLoader` exists,
   `module.__annotations__` works; ≥ 7 of the ~13 WS3 rows flip
   (including `test_import` and `test_types`).
5. `_decimal` passes the `decTest` corpus via `test_decimal` as a
   measured row (residuals enumerated, not skipped).
6. Pickle protocol 5 round-trips out-of-band buffers;
   `test_pickle`, `test_picklebuffer`, `test_pickletools` are
   measured rows with `test_picklebuffer` green.
7. Zero `timeout` rows remain; any slow-but-correct suite carries a
   measured budget override with `status = "pass"`.
8. The final sweep shows **≥ 55 net red→green flips** (pass count
   ≥ 473/542, from 418), no regressions, `unexpected 0`, and the
   ecosystem lane still 27/27 offline.
9. `cargo fmt` / `clippy -D warnings` / `cargo test --workspace` /
   `regrtest --check` / `ecosystem --check` all green.

## Drawbacks

- **Breadth over depth risk.** Ten workstreams invite shallow
  passes. Mitigation: the measured-first discipline — every
  cluster's exit criterion is a flipped or re-measured row, and
  acceptance 8's net-flip floor keeps the wave honest even if
  individual clusters under-deliver.
- **`_decimal` from-scratch is the largest single artifact** and its
  correctness bar (the decTest corpus) is unforgiving. Mitigation:
  the corpus is *in the vendored suite* — development is
  test-driven against the same oracle that grades acceptance; the
  pure-Python `decimal.py` is a readable reference implementation
  of the identical spec.
- **Peephole parity may pessimize.** Adopting CPython's exact
  emission where suites assert it can discard better codegen.
  Accepted per project goal 1; the RFC 0021/0032 specialization
  layers operate below bytecode shape, so runtime cost is
  negligible.
- **`test___all__`/`test_site`-class rows have long fractal tails.**
  Time-boxed: they are WS3 re-measures, not acceptance-gated flips.

## Alternatives

- **Vendor libmpdec (C) instead of a native-Rust `_decimal`**:
  seriously considered — it is CPython's own answer, and the expat
  precedent (RFC 0056) argues for vendoring. Rejected because
  `_decimal`'s Python-facing layer (contexts, signals-as-exceptions,
  thread-local state, format-spec) is the hard 70% and must be
  written either way; libmpdec's arbitrary-precision core duplicates
  the bigint machinery WeavePy already trusts, and a C tree of
  libmpdec's size (~30 KLOC) enters the workspace for the easy 30%.
  The decTest corpus grades both approaches identically; if the
  native core misses the bar mid-wave, vendoring remains the
  documented fallback.
- **Split this into three waves** (object model; compiler;
  decimal+pickle): rejected — the clusters share re-measure
  dependencies (`co_lnotab` gates four rows across two "waves";
  settrace exactness needs `f_lineno`), and three separate
  full-sweep re-baselines cost more than they de-risk. The
  workstreams are independently landable inside one wave.
- **Skip peephole/`dis` parity as "implementation detail"**:
  rejected — the suites assert it, and RFC 0033 already committed
  to CPython's code-unit form; stopping short leaves four
  permanently-red rows that read as "bytecode is wrong".
- **Stub `tracemalloc.Traceback` and friends as inert shapes**:
  rejected — RFC 0031 wired real allocation events; surfacing them
  through real `Snapshot` statistics is a small delta and the
  difference is observable by real profiling tools, which is the
  RFC 0030 constituency.
- **Grade `test_bigmem`/`test_optimizer`-class rows as principled
  skips now**: deferred to measurement — the policy is "skip only
  what has no public contract"; each such row gets a measured
  attempt first.

## Prior art

- **CPython 3.12/3.13's own comprehension inlining** (PEP 709)
  documents exactly the scope-isolation invariants WS4 restores —
  including the class-body edge cases its implementation tripped on
  in beta, which mirror our measured signature.
- **PyPy** maintains `co_lnotab` as a lazily-synthesized
  back-compat view over its own line table — the WS2 approach —
  and passes `test_decimal` with a from-scratch `_decimal`
  written against the decTest corpus, validating the
  no-libmpdec route.
- **GraalPy** treats `test_ast` node-constructor fidelity as
  table-generated from ASDL, the same mechanization WS2 uses.
- **PEP 574** ships reference tests that `test_picklebuffer`
  imports wholesale; the protocol has no ambiguity left to design.
- **RFC 0048/0051/0053** established the house pattern this wave
  runs at scale: verbatim stdlib steps on a VM gap → minimal
  engine fix → bundled regrtest → row re-measured.

## Unresolved questions

- Whether `test_ast`'s validation cluster requires runtime AST
  *mutation* validation (CPython validates at compile time) — if
  suites probe `ast.AST.__setattr__` invariants we don't hold,
  the row may keep an enumerated residual.
- Whether the `test_sys_settrace` jump tests require *full*
  `frame_setlineno` block-analysis parity (with/without exception
  handlers) in one wave, or whether the common-case validator
  covers the suite's corpus. Measured at implementation time.
- Whether `test_bigmem` can pass meaningfully on CI-sized machines
  (CPython skips most of it below 2.5 GiB limits — our
  `test.support` memory accounting must report honestly).
- `_decimal` performance: the acceptance bar is correctness
  (decTest); if the native core is measurably slower than
  `_pydecimal` on the suite, the row still flips but a perf note
  lands in Future work.

## Results

Measured on macOS arm64 against vendored CPython 3.13, per the
RFC 0049 protocol (full `regrtest --all-cpython --mode subprocess
--jobs 8` sweeps; ecosystem offline lane from
`target/ecosystem-wheels`).

### Headline

| Metric | Before (RFC 0056 baseline) | After |
|---|---|---|
| `Lib/test` sweep | 418 pass / 542 | **496 pass / 543** (fail 41, error 0, skip 6, **timeout 0**), `unexpected 0` |
| Net red→green flips | — | **+78 net** (bar: ≥ 55) |
| Ecosystem lane (offline) | 27/27 | **27/27**, 0 unexpected |
| Gates | — | `cargo fmt` / `clippy -D warnings` / `cargo test --workspace --release` (37 suites, 0 failures) / `regrtest --check` exit 0 / `ecosystem --check` exit 0 |

### Workstream outcomes

| WS | Deliverable | Result |
|---|---|---|
| WS1 | Object-model fidelity burn | Exception `args` as a real slot (incl. `SystemExit` payload printing), slot-descriptor error taxonomy, `int.__new__` subclass allocation, `OSError` subclass `errno`/`strerror` population |
| WS2 | Compiler/AST/bytecode introspection | `compile()` from AST + `PyCF_*` flags; `test_ast` residual **169F/80E → 1F/0E** (single enumerated residual: `ASTConstructorTests.test_non_str_kwarg`) |
| WS3 | Import machinery & module metadata | `AppleFrameworkLoader` + frozen-module specs land; `test_import` and `test_types` go from module-level `ImportError` to running end-to-end (3F/12E and 24F/10E measured rows with enumerated residuals — see below) |
| WS4 | Scope & unpacking semantics | Comprehension-scope root cause fixed; `test_listcomps` / `test_dictcomps` / `test_setcomps` / `test_named_expressions` all pass |
| WS5 | Builtins & numerics edges | `float.hex()` full-precision output, `pow`/`sort`/`range`/`print` edge conformance, `int()` error shapes |
| WS6 | Observability event-exactness | CPython-faithful pattern-match codegen + jump threading with NO_LOCATION eligibility; `pass` lowered to located NOP; `test_sys_settrace` residual 58F → 49F |
| WS7 | `_decimal` | `test_decimal` is a measured **pass** row (decTest corpus, 600 s budget) |
| WS8 | Pickle protocol 5 + `_pickle` | `test_pickle`, `test_picklebuffer`, `test_pickletools` all measured **pass**; out-of-band `PickleBuffer` round-trips (release-poisoning fix in `memoryview` exporter delegation) |
| WS9 | Retire the timeouts | **Zero `timeout` rows.** `test_deque` / `test_mmap` / `test_weakref` pass under measured budget overrides |
| WS10 | Stdlib residuals burn | `test_warnings` / `test_unittest` / `test_faulthandler` / `test_patma` (+ the `match` compiler rewrite) flip; `PUSH_EXC_INFO` handler-tag pyc round-trip fixed (cache tag → `weavepy-313-19`) |
| WS11 | Re-measure & re-baseline | Final sweep `unexpected 0`; expectations rewritten from evidence; three engine bugs fixed during re-baseline (below) |

### Engine bugs found by the re-baseline itself

1. **TLS shutdown drain over-read (`test_ssl` sweep timeouts).** The
   `unwrap()` drain used a greedy `read_tls(sock)`; under sweep load
   the peer's `close_notify` and its *next plaintext message* land in
   one kernel buffer, and the drain consumed both — a STARTTLS-style
   downgrade then deadlocked with both peers blocked in `recv` on
   empty queues (`test_starttls`, ~3–5% repro under 6-way stress).
   The drain is now record-precise (`RecordReader`); 600/600 stress
   iterations clean, suite ~15 s under the harness.
2. **`datetime.datetime_CAPI` stand-in shadowed the real capsule.**
   WS3's Python-level `PyCapsule` stand-in (for `types.CapsuleType` /
   `test_types` module-scope import) made `PyCapsule_Import` resolve a
   non-capsule and return NULL — any extension doing
   `PyDateTime_IMPORT` (orjson, numpy) segfaulted at init.
   `PyCapsule_Import` now mints and installs the real well-known
   capsule over a non-capsule attribute; the ecosystem lane is back to
   27/27.
3. **`weavepy-conformance` fingerprint corruption after disk-full.**
   Interrupted builds left cargo believing the bin was fresh while the
   link output was missing (process-level issue, not code; fixed by
   `cargo clean -p` + relink).
4. **Daemon-thread shutdown kill fired on foreign host threads.** The
   dispatch loop's `tstate_must_exit` analogue killed *any* non-main
   thread once the process-global `FINALIZING` flag was set — but the
   "main thread" is claimed once, by whichever thread boots the first
   interpreter. A host embedding several interpreters on its own
   threads (`cargo test` running `run_source` calls in parallel) had
   one interpreter's teardown raise a spurious silent `SystemExit`
   inside another's main module (`run_empty_source_succeeds`, ~30%
   flaky). The kill is now scoped to threads WeavePy's own
   `_thread.start_new_thread` spawned (0/30 after, daemon-kill
   semantics verified intact via `test_io`/`test_threading`).
5. **Per-module import lock had a seed-before-mark window
   (bpo-34572).** The loader inserted the module shell into
   `sys.modules` *before* marking it initializing, and the importer
   checked the mark *before* reading the cache — under sweep load a
   concurrent `pickle.loads` grabbed the half-initialized module
   (`test_pickle.test_unpickle_module_race`,
   `AttributeError: module 'locking_import' has no attribute
   'ToBeUnpickled'`, ~2% repro). The mark now precedes the seed in all
   three loaders (file / frozen-source / meta-path) and `load_one`
   re-checks the holder after the cache read; 0/900 across plain and
   6-way-loaded stress.

### Notable residuals (enumerated, not blockers)

- `test_types` (24F/10E): PEP 604 `Union` runtime semantics
  (hash/instancecheck/GenericAlias interop), `SimpleNamespace`
  repr/replace/constructor edges, `mappingproxy` constructor+methods,
  coroutine duck-typing wrappers, `__format__` locale edges,
  `test_internal_sizes`.
- `test_import` (3F/12E): SubinterpImportTests need the
  `_testsinglephase`/`_testmultiphase` C fixtures; frozen-module
  from-import error shape; `PycRewritingTests.test_foreign_code`.
- `test_sys_settrace` (49F): remaining `frame_setlineno`
  block-analysis parity and a tail of event-exactness cases.
- `test_ast` (1F): `ASTConstructorTests.test_non_str_kwarg`.
- `test_zoneinfo` (4): C-extension implementation-detail residuals
  (weak-cache corruption trio + `test_cache_location`).

### Acceptance checklist

1. Comprehension-scope root cause fixed, quartet flipped — **met**.
2. `f_lineno` / exception-`args` slot / slot-descriptor taxonomy with
   bundled regrtests — **met**.
3. `compile()` from AST + `PyCF_*`; `test_ast` internal count < 25 —
   **met** (1F/0E).
4. Frozen specs + `AppleFrameworkLoader` + `module.__annotations__`;
   `test_import`/`test_types` flip — **partial**: both suites now run
   end-to-end (previously module-level ImportError) but remain
   measured-fail rows with the residuals enumerated above.
5. `_decimal` via decTest as a measured row — **met** (pass).
6. Pickle protocol 5 round-trips; trio measured, `test_picklebuffer`
   green — **met** (all three pass).
7. Zero timeout rows — **met**.
8. ≥ 55 net flips (≥ 473 pass), no regressions, `unexpected 0`,
   ecosystem 27/27 — **met** (496 pass, +78 net).
9. fmt / clippy / cargo test / `regrtest --check` /
   `ecosystem --check` — **met**.
