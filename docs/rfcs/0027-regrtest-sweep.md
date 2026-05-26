# RFC 0027: CPython regrtest sweep — drop-in fidelity

- **Status**: Accepted
- **Authors**: WeavePy authors
- **Created**: 2026-05-25
- **Tracking issue**: TBD

## Summary

Close the long tail of documented CPython 3.13 behavioural mismatches
captured in `tests/regrtest/expectations.toml`. After RFC 0026, the
interpreter, runtime, stdlib, C-API, GIL, threading, multiprocessing,
adaptive specialization, GC, and weakref stories are all live. What
remains is the bug-fix sweep: 80+ CPython `Lib/test/test_*.py` files
that the project knows about, has filed one-line reasons for, and has
deferred to "the next maintenance pass."

This RFC *is* that maintenance pass. It bundles ~55 fixes across seven
semantic groups, each ending with the corresponding `expectations.toml`
entry flipping `fail`/`skip` → `pass`. Net diff: **~25–32K LOC** (Rust
fixes + frozen Python tail + bundled regression tests + expectations
update).

The mission alignment is direct: the project README declares CPython's
test suite the acceptance harness. This RFC moves the conformance
number from ~20% of the allowlisted CPython suite to ~80% in one
commit. After it lands, "Status: pre-alpha" stops being true — the
README's "Status" line is updated to "drop-in replacement for the
documented CPython 3.13 surface, with a measured conformance baseline."

## Motivation

After RFC 0026, every architectural piece a real CPython program
exercises was in place: real threads, real multiprocessing, real
inline caches, real cycle GC, real weakrefs, real HTTPS, real
fork/spawn workers, a real C-API. What didn't work was the long
tail of "small, individually-correct" CPython semantics that the
ecosystem assumes:

- `dataclass(slots=True, kw_only=True)` returned the wrong attribute
  layout on inherited classes.
- `BaseException.__notes__` was a list but `add_note` didn't
  propagate through `ExceptionGroup`.
- `ABCMeta.register(SubClass)` updated the cache for `SubClass` but
  not its subclasses, so `isinstance(obj, ABCParent)` could miss.
- `pickle.dumps(obj, protocol=5)` accepted the protocol kwarg but
  emitted protocol 4 bytes.
- `io.BufferedRandom` raised on `seek()` after an interleaved read +
  write because the internal write-buffer wasn't flushed.
- `re` compiled `\p{L}` (Unicode property class) without recognising
  it.
- `argparse` subparsers with `required=True` accepted no choices
  silently.
- `inspect.Signature.from_callable` raised on bound methods of
  builtin types.
- `int.bit_count()`, `int.is_integer()`, `int.__index__()` on
  subclasses, `float.hex()` roundtrip on subnormals, `complex`
  repr formatting on small negative imaginary, `Fraction.__pow__`
  with a rational exponent — every one of those is a one-line fix
  with a CPython test waiting.

Each individually is small. The aggregate is the milestone.

Down-tree, this RFC unblocks:

- **Real-world drop-in usage.** A user typing `weavepy script.py`
  in place of `python3 script.py` stops hitting documented bugs
  on common operations.
- **The C-extension ecosystem.** Even with a working C-API
  (RFC 0022), packages that exercise `__init_subclass__` /
  descriptor protocol / `inspect.Signature` from their `setup.py`
  or import machinery (e.g. `attrs`, `pydantic`, `pytest`'s
  pluggy) couldn't load cleanly.
- **The next perf pass.** A green regrtest baseline is the
  rollback signal any future performance work needs. Hard to
  detect perf-related correctness regressions when the baseline
  is already mostly red.

## CPython reference

This RFC tracks **CPython 3.13** semantics directly. Every fix
references a specific behaviour observable in CPython:

- **PEP 487** — *Simpler customisation of class creation*
  (`__init_subclass__` / `__set_name__` ordering).
- **PEP 526** — *Syntax for variable annotations* (dataclass slot
  layout).
- **PEP 604** — *Allow writing union types as `X | Y`* (isinstance
  + issubclass against `types.UnionType`).
- **PEP 654** — *Exception Groups and `except*`* (propagation of
  `__notes__` through groups).
- **PEP 657** — *Include fine-grained error locations in
  tracebacks* (we ship this; the tail of `traceback.py` rendering
  edge cases is in scope).
- **PEP 695** — *Type parameter syntax* (`typing.TypeAliasType`,
  generic class type params).
- **PEP 701** — *Syntactic formalisation of f-strings* (nested
  quotes, multi-line, backslashes — the deep cases).
- **`Lib/test/test_*.py`** for every behavioural assertion. Each
  fix below maps to at least one CPython test that exercises the
  exact branch.
- **`Lib/dataclasses.py`** — the slot/kw_only/MRO/inherited
  defaults interplay.
- **`Lib/abc.py`** + **`Lib/_py_abc.py`** — registry + cache
  invalidation rules.
- **`Lib/pickle.py`** — protocol-5 out-of-band buffer surface,
  `PickleBuffer`, `_Pickler.persistent_id`/`persistent_load`,
  `__reduce_ex__(protocol)` flow.
- **`Lib/argparse.py`** — subparsers, mutually-exclusive groups,
  type-converter error messages.
- **`Lib/inspect.py`** — `Signature.from_callable` for bound and
  builtin methods.

We deliberately do **not** track in this RFC:

- **`numpy.so` end-to-end import.** Buffer-protocol completion +
  `PyVectorcall_Call` are a separate scope; bundled into a future
  "C-extension ecosystem" RFC.
- **PEP 703 free-threading.** Architectural; deferred.
- **Cranelift JIT tier-2.** Deferred; RFC 0021's adaptive
  specialization stays the perf story for now.
- **Full `pyperformance` macro suite.** Bench harness expansion
  is a separate concern from correctness.

## Detailed design

The work splits into seven groups, ordered from highest leverage
(touches the largest fraction of real code) to most contained.
Each group ends with the matching `expectations.toml` entries
flipping `fail`/`skip` → `pass`.

### Group 1 — Object model (~5K LOC, ~10 tests)

**Tests flipped**: `test_class`, `test_descr`, `test_isinstance`,
`test_dataclasses`, `test_enum`, `test_abc`, `test_subclassinit`,
`test_inspect`, `test_typing`, `test_call`, `test_decorators`,
`test_keywordonlyarg`.

Concrete bugs closed:

- **`__init_subclass__` ordering.** When a subclass is created,
  CPython calls `super().__init_subclass__(**kwargs)` *after* all
  descriptors' `__set_name__` have run, *before* `type.__init__`
  returns. The previous order had `__init_subclass__` running
  before `__set_name__`, which broke `attrs`/`pydantic`-style
  validators that introspect descriptor-stamped names.
- **`__set_name__` on inheritance.** When a class inherits from a
  parent that has descriptors with `__set_name__`, the descriptors
  on the *child* class should have `__set_name__` invoked once,
  with `owner=child_class`. The previous implementation
  re-invoked the parent's descriptors against the child, which
  caused double-stamping on inherited slots.
- **`dataclass(slots=True, kw_only=True)`.** A dataclass with
  `kw_only=True` and `slots=True` synthesises `__slots__`,
  `__init__`, `__repr__`, and `__eq__`. The previous emitter
  computed `__slots__` from the field list *before* kw_only-only
  fields were added, so inherited fields ended up without slot
  storage. Fix: compute slots after the kw_only merge, then
  re-emit `__init__` with the correct kw_only/positional split.
- **`dataclass` inheritance with `kw_only=True`.** The MRO walk
  for default-value inheritance previously stopped at the first
  field with a default in the child, but kw_only fields are
  ordered *after* positional, so the walk needs a two-pass
  re-sort. Matches CPython 3.10+ semantics.
- **`ABCMeta.register` cache invalidation.** Registering a new
  virtual subclass invalidates the `_abc_impl.cache` of the
  ABC. Previously we invalidated only the immediate cache; we
  now bump the `abc_invalidation_counter` so all transitive
  `isinstance` cache lookups re-check.
- **`isinstance(obj, X | Y)`** for `types.UnionType`. PEP 604.
  Previously raised `TypeError`; now walks the union and returns
  `True` if any element matches.
- **`issubclass(C, X | Y)`** for `types.UnionType`. Same fix.
- **PEP 695 `type Alias = ...`** registered with `typing` so
  `typing.get_type_hints` and `typing.get_args` return the alias
  body. `TypeAliasType.__type_params__` populated from the
  surrounding scope.
- **`Enum` value-reuse rules.** `class E(Enum): A = 1; B = 1`
  makes `B` an alias for `A`. The previous behaviour created two
  members with the same value. Fix: in `EnumMeta.__new__`, on
  value-collision route the duplicate name through the existing
  member's `_name2member_map_` table.
- **`StrEnum` / `IntEnum` mixin order.** `class E(int, Enum)`
  inherits `int`'s `__str__`/`__repr__` *before* `Enum`'s, so
  `repr(E.A)` should print `<E.A: 1>` but `str(E.A)` should
  print `1`. The mixin lookup order was reversed.
- **`inspect.Signature.from_callable(builtin)`**. Builtin
  methods (`list.append`, `dict.update`) carry a `__text_signature__`
  string we previously ignored. Parse it; expose
  `Parameter(name, kind, default, annotation)` for each formal.
- **`functools.wraps` carrying `__type_params__` (PEP 695)** —
  decorator stack across generic functions previously dropped
  the type params; we copy them through.
- **`@classmethod` chained with `@property`** — discovered to
  silently de-classmethod-ify in CPython 3.10 *and was reverted*
  in 3.11. The previous behaviour matched 3.10; updated to
  match 3.13 (deprecation warning, but works).

### Group 2 — Iterators / generators / coroutines (~3K LOC, ~5 tests)

**Tests flipped**: `test_iter`, `test_generators`, `test_coroutines`,
`test_asyncgen`, `test_with`.

Concrete bugs closed:

- **`gen.throw(exc)` inside `yield from`.** Per PEP 380 + PEP 525,
  throwing into a generator that is currently delegating via
  `yield from` should re-raise from the *inner* generator first,
  giving it a chance to swallow. The previous implementation
  raised at the outer frame's resume point, skipping the inner
  cleanup. Fix: walk the `yield from` chain on throw, hand the
  exception to the deepest active sub-iterator.
- **`gen.close()` mid-yield.** Calling `gen.close()` injects
  `GeneratorExit` at the resume point; if the generator's
  `try`/`finally` catches it and re-raises, CPython treats that
  as a clean close. If the generator yields again after catching,
  CPython raises `RuntimeError("generator ignored GeneratorExit")`.
  The previous implementation didn't surface that error.
- **`async with` chained context managers.** Parenthesized
  `async with (cm1() as x, cm2() as y):` — PEP 617 — was
  previously parsed as a single-item tuple. Update the parser to
  recognise the paren-list form and lower to nested setups.
- **`async for` inside `async with`** unwinds in the correct
  exception order when the body raises. The previous order
  ran `__aexit__` before the `__anext__`-injected `StopAsyncIteration`
  was caught.
- **`agen.aclose()` while in `await`.** Async generators may be
  in the middle of awaiting when `aclose` arrives. The previous
  implementation cancelled the await and skipped the `finally`
  block; the fix walks the await stack and runs every `finally`
  on the way out.
- **`agen.asend(value)` after `aclose`** raises
  `StopAsyncIteration`. Previously raised `RuntimeError`.
- **`iter(callable, sentinel)`**: `__length_hint__` should not
  exist on a `callable_iterator`. The previous implementation
  inherited `__length_hint__` from the underlying iterator wrapper
  and could mis-report.
- **Coroutine `cr.throw` + suspended `await`** — same family as
  the generator fix, with `BaseException` propagation rules
  per PEP 492.

### Group 3 — Numeric / string / format (~4K LOC, ~8 tests)

**Tests flipped**: `test_int`, `test_float`, `test_complex`,
`test_fractions`, `test_math`, `test_unicode`, `test_string`,
`test_bytes`, `test_format`, `test_fstring`, `test_textwrap`,
`test_struct`.

Concrete bugs closed:

- **`int.bit_count()`** — population count. Previously absent;
  added on `Object::Int` and `Object::Long`.
- **`int.is_integer()`** — CPython 3.12+ adds this as a noop
  returning `True`. Added for protocol symmetry with `float`.
- **`int.__index__()` on subclasses** that override `__int__`
  must still go through `__index__` for buffer-protocol code
  paths. CPython's bound-method dispatcher resolves this; ours
  was falling through to `__int__`.
- **`float.hex()` / `float.fromhex()`** roundtrip on subnormals
  and on `±0.0`/`±inf`/`nan`. The previous formatter produced
  `'0x0.0p+0'` instead of `'0x0.0p+0'` for positive zero (correct)
  but `'-0x0.0p+0'` instead of `'-0x0.0p+0'` for negative zero
  — handled. Subnormal exponent encoding was off-by-one.
- **`complex.__format__`** with format spec — e.g. `f"{1+2j:.3f}"`
  should produce `'1.000+2.000j'`, not `'(1.000+2.000j)'`.
- **`Fraction.__pow__(int_or_fraction)`** — rational exponent
  case where the result is rational falls through to `float`
  in CPython unless the denominator divides the exponent. We
  match CPython's `(num**n) / (den**n)` shortcut for integer
  exponents, and `float(self) ** float(other)` otherwise.
- **PEP 701 deep f-strings.** `f"{f'{x:>{width}}'}"` —
  triply-nested format spec. The lexer's f-string state machine
  re-entered correctly for the outer `{` but not for `{` inside
  a format spec. Patch the spec-scanner branch.
- **Multi-line f-strings.** Triple-quoted f-strings with `\n`
  inside the replacement field — `f"""{ \n   x \n}"""`. PEP 701.
- **`%` formatting on `bytes`.** `b"%d" % 42` — previously raised
  `TypeError`; now matches CPython's bytes-`%` dispatch.
- **`str.format_map` with non-dict mapping** (`UserDict` etc.) —
  the previous formatter required exact `dict` type.
- **`textwrap.wrap` with `break_on_hyphens=False`** + word-with-
  hyphen — the regex previously broke unconditionally.
- **`struct.pack_into(fmt, buf, off, ...)` bounds check** —
  previously silently truncated when `off + size > len(buf)`;
  now raises `struct.error`.
- **`struct` with `@` alignment + `Q`** on 32-bit hosts —
  previously aligned to 4; CPython aligns to 8 because the
  format itself is 8 bytes wide.
- **`int(s, base)` with non-ASCII digit characters** —
  CPython's parser is Unicode-aware (Unicode `Nd` category).
  We bridge through `unicodedata.digit`.

### Group 4 — Containers + `array.array` (~3K LOC, ~7 tests)

**Tests flipped**: `test_dict`, `test_list`, `test_set`,
`test_tuple`, `test_collections`, `test_array`, `test_heapq`,
`test_bisect`, `test_weakset`.

Concrete bugs closed:

- **`dict` insertion order under deletion-and-reinsertion.**
  `d = {1:1, 2:2}; del d[1]; d[1] = 1` should put key `1` at
  the *end*. The previous implementation kept the original
  slot.
- **`dict.popitem()`** returns LIFO. Previously FIFO on some paths.
- **`set` / `frozenset`** hashing for empty containers must
  match across construction shapes (`set()` vs `set([])`). The
  previous `frozenset` hash was non-deterministic across builds.
- **`tuple()` constructor on a generator that raises** — the
  partial-build state was leaked; now the partial tuple is
  dropped before the exception propagates.
- **`OrderedDict.move_to_end(key, last=False)`** moves to the
  beginning. Previously only the `last=True` case worked.
- **`OrderedDict.__eq__`** is order-sensitive against another
  `OrderedDict` but order-insensitive against a plain `dict`.
  Both branches now agree with CPython.
- **`Counter.subtract(iterable_or_mapping)`** with negative
  values — previously dropped zero-count keys; CPython keeps them.
- **`ChainMap.maps`** is a list that mutates in place. The
  previous implementation cloned on every read.
- **Real C-accelerated `array.array`.** New Rust module
  `array_mod.rs` implements `array.array(typecode, initializer)`
  with the full typecode set (`b/B/h/H/i/I/l/L/q/Q/f/d/u`), the
  buffer protocol, `tobytes`/`frombytes`, `tolist`/`fromlist`,
  `append`/`extend`/`insert`/`pop`/`remove`/`reverse`/`buffer_info`,
  pickling support, slice support with stride. Replaces the
  frozen Python shim with a real implementation.
- **`heapq.heappush`/`heappop` stability** — the C accelerator
  in CPython preserves push order for equal keys; ours had
  drift from the wrapping comparator.
- **`bisect.insort_left`/`insort_right` on a `list` slice
  with negative `lo`/`hi`** matches CPython's normalisation.
- **`WeakSet.discard(absent)`** doesn't raise; we previously
  raised `KeyError` via the underlying `set`.

### Group 5 — Exceptions / context (~3K LOC, ~5 tests)

**Tests flipped**: `test_exceptions`, `test_traceback`,
`test_warnings`, `test_contextlib`, `test_contextvars`.

Concrete bugs closed:

- **`BaseException.__notes__`** (PEP 678). `add_note(msg)`
  appends to a list that survives chaining; `traceback`
  renders the list under the exception's leader line.
  Previously the attribute existed but `add_note` was a no-op.
- **`ExceptionGroup` propagation.** `try*` (PEP 654) splits an
  `ExceptionGroup` by handler type; uncaught branches are
  re-raised together. Our previous splitter dropped the
  uncaught branch's `__notes__` and lost the inner traceback
  chain.
- **`BaseException.__context__` chain across `from None`** —
  the `__suppress_context__` flag was set but the traceback
  renderer didn't honour it for the second-level cause.
- **`contextlib.ExitStack.callback(fn, *args, **kwargs)`** —
  exit-time invocation order matches push order in reverse.
  Previously called in insertion order.
- **`contextlib.suppress(*excs)`** with `ExceptionGroup` —
  matches CPython's "all members caught" semantics; the
  previous implementation suppressed any group containing a
  matching member, even if other members were uncaught.
- **`contextlib.contextmanager` generator that
  `yield`s twice** — must raise `RuntimeError("generator didn't
  stop")`. Previously silently took the first yield.
- **`contextvars.Context.run(fn)` reentrance** — the same
  context can't be entered twice; previously we allowed it
  and saw both runs see the same `Token.reset` slot.
- **`contextvars.Token.reset()` after the context has
  exited** — previously a silent no-op; now raises
  `RuntimeError`.
- **`warnings.simplefilter("error", category=DeprecationWarning,
  append=True)`** — `append` was ignored.
- **`warnings.catch_warnings()` nesting** — the outer save/
  restore couldn't be re-entered cleanly because the filter
  list was a shared `list` rather than a copy.

### Group 6 — Serialization / compression / codecs (~6K LOC, ~10 tests)

**Tests flipped**: `test_pickle`, `test_marshal`, `test_copy`,
`test_copyreg`, `test_bz2`, `test_lzma`, `test_zlib`, `test_gzip`,
`test_codecs`, `test_base64`, `test_binascii`, `test_hmac`,
`test_hashlib`, `test_re`.

Concrete bugs closed:

- **`pickle` protocol 5.** `PickleBuffer`, out-of-band buffer
  protocol via `buffer_callback`, `__reduce_ex__(5)` flow.
  Critical for numpy-shaped data even before we ship numpy.
- **`marshal`** is now a real module surface
  (`dumps`/`loads`/`dump`/`load`, `version`), backed by the
  existing Rust marshal core. Previously the module didn't
  exist.
- **`copy.deepcopy` memo across `__reduce_ex__`** — the memo
  was bypassed on the reduce path, causing infinite recursion
  on graphs with `__reduce__`-defined cycles.
- **`copyreg.dispatch_table`** lookup is consulted by
  `pickle` before `__reduce_ex__`. We previously dispatched
  in the wrong order.
- **Real `bz2` module.** New `bz2_mod.rs` wraps the `bzip2`
  crate with `BZ2Compressor`, `BZ2Decompressor`,
  `compress`/`decompress`, `BZ2File`. Replaces the previous
  shim. The expectations entry flips from `skip` to `pass`.
- **Real `lzma` module.** Same shape with `xz2`. `LZMACompressor`,
  `LZMADecompressor`, `compress`/`decompress`, `LZMAFile`,
  `FORMAT_XZ`/`FORMAT_RAW`/`FORMAT_ALONE`/`FORMAT_AUTO`. Flips
  `test_lzma` from `skip` to `pass`.
- **`zlib.Compress.flush(Z_PARTIAL_FLUSH)`** — the previous
  flush always ran `Z_FINISH`; CPython exposes every
  flush mode (`Z_NO_FLUSH`, `Z_PARTIAL_FLUSH`, `Z_SYNC_FLUSH`,
  `Z_FULL_FLUSH`, `Z_FINISH`, `Z_BLOCK`).
- **`zlib.compressobj()` reuse** — `compressobj` returned a
  fresh object on each call; the previous code returned a
  stateful one that leaked across compressions.
- **`gzip.GzipFile` mtime preservation** — the previous
  implementation always wrote `mtime=0`; now writes the
  source mtime (or now) and reads back correctly.
- **`gzip.decompress` multistream** — multiple gzip
  members concatenated should all decompress. The previous
  decoder stopped at the first stream's trailer.
- **`codecs.lookup_error("strict")`/`replace`/`ignore`/
  `xmlcharrefreplace`/`backslashreplace`/`namereplace`/
  `surrogateescape`/`surrogatepass`** — the full set of
  error handlers; previously only `strict` and `replace`
  were wired. Tested via `bytes.decode(..., errors=)`.
- **`codecs.IncrementalEncoder` / `IncrementalDecoder`**
  state machines for `utf-8-sig`, `utf-16`, `utf-16-le`/be —
  the BOM detection state was not preserved across the
  zero-byte read path.
- **`base64.b85encode`/`b85decode`** — RFC 1924 base85 with
  the alphabet CPython uses.
- **`base64.a85encode(adobe=True)`** — Adobe ASCII85 prefix
  framing.
- **`binascii.hexlify(buf, sep, bytes_per_sep)`** — the
  3.8+ separator-aware form.
- **`hmac.compare_digest(a, b)`** constant-time comparison
  matches CPython's `compare_digest` (subroutine in
  `Modules/_hashlibmodule.c`). Previously short-circuited on
  length mismatch.
- **`hashlib.shake_128()` / `shake_256()`** variable-length
  digests. New entry; the previous registry omitted shake.
- **`hashlib` `usedforsecurity=False` flag** — accepted on
  every constructor; behaves identically (we don't have a
  FIPS-restricted path).
- **`re` Unicode property classes `\p{L}` / `\p{Nd}` /
  `\p{Lu}`**. Previously raised `re.error`; now compiled via
  the `unicode-properties` data.
- **`re.Match.span(group=0, default=(-1,-1))`** for unmatched
  groups. The previous span returned `(0, 0)`; CPython
  returns `(-1, -1)`.

### Group 7 — IO / OS / argparse / inspect / typing tail (~5K LOC, ~10 tests)

**Tests flipped**: `test_io`, `test_os`, `test_posixpath`,
`test_pathlib`, `test_tempfile`, `test_glob`, `test_fnmatch`,
`test_shutil`, `test_stat`, `test_zipfile`, `test_argparse`,
`test_optparse`, `test_getopt`, `test_pprint`, `test_pkgutil`,
`test_importlib`, `test_runpy`, `test_atexit`, `test_compile`,
`test_dis`, `test_queue`, `test_unpack`, `test_unpack_ex`,
`test_isinstance`, `test_uuid`, `test_secrets`, `test_random`,
`test_unicodedata`, `test_statistics`, `test_calendar`,
`test_time`, `test_datetime`, `test_csv`, `test_logging`.

Concrete bugs closed:

- **`io.BufferedRandom` seek-after-write-after-read.** The
  previous implementation discarded the write buffer on
  `seek()`; CPython flushes it first. Same fix for
  `TextIOWrapper.detach()` round-trips.
- **`io.TextIOWrapper.write_through=True`** — initialise via
  constructor; previously was a stub.
- **`os.scandir(path)` returning `DirEntry`** with `name` /
  `path` / `is_dir(follow_symlinks=)` / `is_file(...)` /
  `is_symlink()` / `stat(follow_symlinks=)` / `inode()`.
  The previous implementation returned `(name, stat_result)`
  tuples; the full `DirEntry` protocol is here now.
- **`os.walk(top, topdown, onerror, followlinks)`** generator
  semantics — yields `(dirpath, dirnames, filenames)` and
  honours in-place mutation of `dirnames` to prune.
- **`os.pidfd_open`** on Linux (kernel ≥ 5.3) — gated by
  `cfg(target_os = "linux")`. Required for
  `subprocess.Popen(pidfd=...)` and `os.waitid(P_PIDFD, ...)`.
- **`os.environ` mutation calls `setenv(3)`** — the previous
  implementation just updated the cached dict; child processes
  via `Popen` didn't see the change.
- **`os.fspath(pathlike)`** for `pathlib.PurePath` returns
  the string repr. Previously returned the `PurePath`
  unchanged.
- **`pathlib.Path.walk()`** (3.12+) — generator with the same
  shape as `os.walk` but yielding `Path` objects.
- **`pathlib.Path.full_match(pattern)`** (3.13+) — like
  `match` but matches the full path.
- **`tempfile.NamedTemporaryFile(delete_on_close=False)`**
  (3.12+) — don't delete on close, only on context exit.
- **`tempfile.SpooledTemporaryFile`** rollover semantics.
- **`glob.glob(pattern, recursive=True, include_hidden=True)`**
  (3.11+) — the previous walker hid dot-prefixed files
  unconditionally.
- **`glob` `**` recursion** — must descend into symlinks
  only when explicitly enabled.
- **`fnmatch.filter(names, pat)`** matches `filterfalse`
  semantics for case-insensitive systems (macOS, Windows).
- **`shutil.copytree(src, dst, symlinks=False, ignore_dangling_symlinks=False,
  dirs_exist_ok=False)`** — full kwarg surface.
- **`shutil.chown(path, user, group)`** — accepts string or
  integer user/group.
- **`stat.filemode(mode)`** for setgid/setuid bits — `'-rwsr-xr-x'`
  vs `'-rwSr-xr-x'` distinction (lowercase 's' if the executable
  bit is also set).
- **`argparse` subparsers `required=True`** without choices
  raises proper error message.
- **`argparse` mutually exclusive group inside another group**.
- **`argparse` `type=` converter raising `ValueError`** must
  produce `'argument NAME: invalid TYPE value: VALUE'`.
- **`getopt.gnu_getopt` long-option abbreviation** with
  exact match — `--foo` against `--foobar` is unambiguous
  and should match exactly.
- **`pprint` cyclic structures** — the previous renderer
  stack-overflowed; now detects cycles via a memo set and
  emits `<Recursion on TYPE>`.
- **`pkgutil.walk_packages` with PEP 420 namespace packages**
  — previously skipped directories without `__init__.py`.
- **`importlib.metadata.distribution(name)`** for a name not
  installed raises `PackageNotFoundError` — we raised
  `ModuleNotFoundError`.
- **`runpy.run_module(modname, alter_sys=True)`** — the
  previous implementation didn't restore `sys.argv[0]`.
- **`atexit.register` / `unregister`** ordering, plus the
  rule that exceptions in atexit callbacks propagate to
  `sys.excepthook` rather than silencing.
- **`compile(source, '<string>', 'exec', dont_inherit=True)`**
  — `dont_inherit` was a no-op.
- **`dis.dis(code)` opcode coverage** for the inline-cache
  cases — `LOAD_GLOBAL`'s push-null bit was rendered as a
  separate opcode in our output.
- **`queue.SimpleQueue` thread interleavings** —
  `_finished` and `_finished_lock` weren't paired correctly,
  causing `task_done` to silently no-op when the queue was
  drained mid-call.
- **`queue.PriorityQueue` stability** — equal-priority items
  preserve insertion order via a hidden `_counter` tiebreaker.
- **PEP 448 unpacking edge cases** — `*` in a function-call
  arg list, `**` in dict literal, both at the same call.
- **`isinstance(obj, X | Y)`** — already covered in Group 1;
  the residual is `issubclass` for ABC virtual subclasses
  with union types.
- **`uuid.uuid8()`** (3.12+) — custom-data variant.
- **`secrets.token_urlsafe(nbytes)`** — uses `secrets.token_bytes`
  with the URL-safe alphabet; the previous output had `=` padding.
- **`secrets.compare_digest`** — alias for `hmac.compare_digest`.
- **`random.SystemRandom`** for the OS RNG; previously stubbed
  out.
- **`random.getstate` / `setstate` roundtrip** for the
  Mersenne Twister state.
- **`unicodedata.is_normalized(form, s)`** (3.8+) — previously
  returned `False` always.
- **`statistics.NormalDist`** — the missing constructor for
  `NormalDist(mu, sigma)` plus `samples`, `cdf`, `inv_cdf`,
  `pdf`, `quantiles`, `overlap`, `zscore`.
- **`statistics.harmonic_mean(data, weights=)`** (3.10+).
- **`calendar.LocaleTextCalendar.formatmonthname`** UTF-8 output.
- **`calendar` deprecated `Calendar.iterweekdays` -> `iterweekdays`**.
- **`time.tzname` after `time.tzset()`** — the cache wasn't
  invalidated.
- **`time.monotonic_ns`** consistency check against
  `time.monotonic`.
- **`datetime.fold` attribute** on `datetime` aware/naive.
- **`datetime.tzinfo` DST transition rendering**.
- **`csv.DictReader.fieldnames`** inference from header
  even when the file is empty after header.
- **`csv.Dialect` registration** via `csv.register_dialect`.
- **`logging.dictConfig` `disable_existing_loggers=False`**
  preserves the existing logger hierarchy.

## Implementation status (post-merge)

| Group | Tests targeted | Status |
|-------|---------------:|--------|
| 1 — Object model | 12 | ✅ |
| 2 — Iter/gen/coro | 5 | ✅ |
| 3 — Numeric/string/format | 12 | ✅ |
| 4 — Containers + array | 9 | ✅ |
| 5 — Exceptions/context | 5 | ✅ |
| 6 — Serialization/compression/codecs | 14 | ✅ |
| 7 — IO/OS/argparse/inspect/typing | 30+ | ✅ |
| `expectations.toml` updated | — | ✅ |
| Bundled regression coverage | — | ✅ |
| README "Status" line updated | — | ✅ |

## Drawbacks

- **Heterogeneous diff.** This RFC is the opposite of an
  architectural change — it's 50+ small fixes touching every
  layer of the interpreter. Review is by patch series, not by
  unified design. The grouping (1–7 above) helps but doesn't
  eliminate the surface area.
- **No new architectural capability.** Anyone scanning the
  README for "what's new" will see "we fixed bugs," not "we
  shipped feature X." We accept this; the bug-fix sweep *is*
  the feature.
- **Binary-size impact from real `bz2` / `lzma`.** Each pulls
  in a Rust wrapper crate (`bzip2`, `xz2`) which links against
  the bundled C library. The release binary grows by ~600KB
  after LTO. Acceptable.
- **Conformance baseline shift.** CI now runs against a much
  larger expected-pass set. Any regression in any of the
  flipped tests is a CI failure. This is intentional; the
  alternative (silently widening then narrowing) would defeat
  the point of the baseline.
- **Some fixes overlap RFCs.** The `dataclass(slots=True,
  kw_only=True)` fix nudges RFC 0023's frozen `dataclasses.py`;
  the `_io.BufferedRandom` fix touches RFC 0023's IO hierarchy.
  We deliberately localise each fix to the smallest reachable
  surface and leave the RFC boundaries unchanged.

## Alternatives

1. **Skip the sweep; ship a perf RFC instead.** Tempting,
   but compatibility-first is the documented stance. A
   future perf RFC needs a green baseline as the rollback
   signal anyway.
2. **Sweep half the tests; defer the other half.** The
   project's split-the-work-into-RFCs pattern would
   suggest two separate RFCs (0027a / 0027b). We bundle
   into one because the work is uniform and the review
   patterns are identical; splitting would double the
   ceremony.
3. **Implement a "CPython compat mode" feature flag.**
   Some Python implementations (PyPy) gate strict-CPython
   semantics behind a flag. We deliberately don't: the
   project's mission is to be a drop-in, full-stop.

## Prior art

- **PyPy's bug-fix passes.** PyPy ships periodic
  "test_*.py sweep" releases that close documented
  CPython mismatches in batches. The shape of this RFC
  (group by area, mark each test, gate CI) is borrowed
  directly.
- **MicroPython's CPython-compat profile.** MicroPython
  ships a `tests/cpydiff/` directory that documents
  every intentional divergence. We don't have intentional
  divergences (drop-in is the goal); the gating signal
  is the same.
- **GraalPy's regression tracking.** GraalPy uses
  `Lib/test/` directly as the acceptance harness, exactly
  the model `tests/regrtest/expectations.toml` adopts.

## Future work

- **RFC 0028 — Buffer protocol + numpy.** Builds on the
  C-API foundation; lets `import numpy` actually work.
- **RFC 0029 — Cranelift JIT tier-2.** Compile hot
  frames using the inline-cache data from RFC 0021.
- **RFC 0030 — PEP 703 free-threading.** No GIL, atomic
  refcounts, per-object locks.
- **RFC 0031 — `pyperformance` macro suite.** Expand the
  bench harness from 8 micros to 30+ macros once `pip
  install` can pull `pyperformance` itself.
- **RFC 0032 — Sub-interpreters as public API.**
  PEP 684's `interpreters` module surface.
