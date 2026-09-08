//! Embedded source manifest shared by the VM and its build script.

/// A frozen-Python module: source compiled at runtime and executed
/// inside the interpreter exactly like a real `.py` file, but without
/// touching the filesystem. Used to ship pure-Python stdlib modules
/// inside the binary.
#[derive(Clone, Copy, Debug)]
pub struct FrozenSource {
    pub name: &'static str,
    pub source: &'static str,
    pub is_package: bool,
}

pub(crate) fn frozen_sources() -> &'static [FrozenSource] {
    // A `static`, not a promoted local: the table is far past clippy's
    // stack-array budget (`large_stack_arrays`).
    static SOURCES: &[FrozenSource] = &[
        // `builtins` is *not* frozen source: the module is created
        // eagerly in `Interpreter::default()` sharing the interpreter's
        // ambient builtins dict (RFC 0052 WS5 — patchable builtins).
        // RFC 0046 (wave 5): `ctypes`. The verbatim CPython `ctypes` package
        // runs over our frozen `_ctypes` reimplementation (CPython's real
        // `_ctypes` is a core-built C extension linking `_PyRuntime`, so it
        // can't be dlopen'd like a stable-ABI wheel). `_ctypes` in turn sits
        // on the native `_ctypes_native` primitive module (memory, dlopen,
        // platform C type sizes, libffi). pandas imports `ctypes`
        // unconditionally (`pandas.errors`).
        // RFC 0076 WS15 — PEP 784: the verbatim CPython 3.14
        // `compression` package (zstd + the bz2/gzip/lzma/zlib
        // re-export shims) over the native `_zstd` core.
        FrozenSource {
            name: "compression",
            source: include_str!("python/compression/__init__.py"),
            is_package: true,
        },
        FrozenSource {
            name: "compression.bz2",
            source: include_str!("python/compression/bz2.py"),
            is_package: false,
        },
        FrozenSource {
            name: "compression.gzip",
            source: include_str!("python/compression/gzip.py"),
            is_package: false,
        },
        FrozenSource {
            name: "compression.lzma",
            source: include_str!("python/compression/lzma.py"),
            is_package: false,
        },
        FrozenSource {
            name: "compression.zlib",
            source: include_str!("python/compression/zlib.py"),
            is_package: false,
        },
        FrozenSource {
            name: "compression._common",
            source: include_str!("python/compression/_common/__init__.py"),
            is_package: true,
        },
        FrozenSource {
            name: "compression._common._streams",
            source: include_str!("python/compression/_common/_streams.py"),
            is_package: false,
        },
        FrozenSource {
            name: "compression.zstd",
            source: include_str!("python/compression/zstd/__init__.py"),
            is_package: true,
        },
        FrozenSource {
            name: "compression.zstd._zstdfile",
            source: include_str!("python/compression/zstd/_zstdfile.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_ctypes",
            source: include_str!("python/_ctypes.py"),
            is_package: false,
        },
        FrozenSource {
            name: "ctypes",
            source: include_str!("python/ctypes/__init__.py"),
            is_package: true,
        },
        FrozenSource {
            name: "ctypes._endian",
            source: include_str!("python/ctypes/_endian.py"),
            is_package: false,
        },
        FrozenSource {
            name: "ctypes._layout",
            source: include_str!("python/ctypes/_layout.py"),
            is_package: false,
        },
        FrozenSource {
            name: "ctypes.util",
            source: include_str!("python/ctypes/util.py"),
            is_package: false,
        },
        FrozenSource {
            name: "ctypes.wintypes",
            source: include_str!("python/ctypes/wintypes.py"),
            is_package: false,
        },
        FrozenSource {
            name: "ctypes.macholib",
            source: include_str!("python/ctypes/macholib/__init__.py"),
            is_package: true,
        },
        FrozenSource {
            name: "ctypes.macholib.dyld",
            source: include_str!("python/ctypes/macholib/dyld.py"),
            is_package: false,
        },
        FrozenSource {
            name: "ctypes.macholib.dylib",
            source: include_str!("python/ctypes/macholib/dylib.py"),
            is_package: false,
        },
        FrozenSource {
            name: "ctypes.macholib.framework",
            source: include_str!("python/ctypes/macholib/framework.py"),
            is_package: false,
        },
        // RFC 0040 WS1 — upgrades the native `os` module's `environ`/
        // `environb` to CPython's write-through `_Environ` mappings. Imported
        // for its side effect immediately after the native `os` module is
        // built (see `Interpreter::load_one`).
        FrozenSource {
            name: "_weave_envinit",
            source: include_str!("python/_weave_envinit.py"),
            is_package: false,
        },
        // RFC 0053 WS2 — builds PEP 451 (spec, loader) pairs lazily for
        // modules the Rust importer loaded natively. See the module
        // `__spec__`/`__loader__` fallback in `Interpreter::load_attr`.
        FrozenSource {
            name: "_weave_spec",
            source: include_str!("python/_weave_spec.py"),
            is_package: false,
        },
        // RFC 0057 WS4 — PEP 667/709 f_locals surface for lowered-
        // comprehension frames (hidden iteration variables). See the
        // `f_locals` arm of `Interpreter::load_attr_inner`.
        FrozenSource {
            name: "_weave_frame_locals",
            source: include_str!("python/_weave_frame_locals.py"),
            is_package: false,
        },
        // RFC 0060 WS1 — the `_testinternalcapi` instruction-sequence
        // fixture (`new_instruction_sequence` / `assemble_code_object`,
        // test_compiler_assemble). The native module delegates here.
        FrozenSource {
            name: "_weave_iseq",
            source: include_str!("python/_weave_iseq.py"),
            is_package: false,
        },
        // RFC 0068 WS1 — the compiler flowgraph stage (a Python port of
        // `Python/flowgraph.c`), backing `_testinternalcapi.optimize_cfg`
        // (test_peepholer's DirectCfgOptimizerTests).
        FrozenSource {
            name: "_weave_flowgraph",
            source: include_str!("python/_weave_flowgraph.py"),
            is_package: false,
        },
        // RFC 0068 WS1 — the compiler codegen stage (a Python port of
        // `Python/compile.c`'s AST lowering), backing
        // `_testinternalcapi.compiler_codegen` (test_compiler_codegen).
        FrozenSource {
            name: "_weave_codegen",
            source: include_str!("python/_weave_codegen.py"),
            is_package: false,
        },
        // RFC 0077 WS14 — code-object introspection behind 3.14's
        // `_testinternalcapi.get_code_var_counts` / `verify_stateless_code`
        // / `get_co_localskinds` / `code_returns_only_none` (test_code).
        FrozenSource {
            name: "_weave_codeinfo",
            source: include_str!("python/_weave_codeinfo.py"),
            is_package: false,
        },
        // RFC 0040 WS7 — CPython's pure-Python `io` reference implementation.
        // `test_io`/`test_fileio` import `_pyio` and exercise *both* the native
        // `io` and `_pyio` side-by-side; without it the whole suite fails to
        // import. Vendored verbatim from CPython (`Lib/_pyio.py`).
        FrozenSource {
            name: "_pyio",
            source: include_str!("python/_pyio.py"),
            is_package: false,
        },
        // RFC 0040 WS7 — the public `io` module: a thin re-export of the native
        // `_io` accelerator, mirroring CPython's real `Lib/io.py`. Preserves
        // type identity (`io.BufferedReader is _io.BufferedReader`) and the
        // shared IOBase ABC family; `_pyio` stays the separate pure-Python twin.
        FrozenSource {
            name: "io",
            source: include_str!("python/io.py"),
            is_package: false,
        },
        // RFC 0040 WS4 — CPython's `signal.py`: layers the `Signals`/
        // `Handlers`/`Sigmasks` IntEnums over the native `_signal` core and
        // wraps `signal`/`getsignal`/`pthread_sigmask`/`sigwait`/
        // `valid_signals` to coerce ints to/from those enums.
        FrozenSource {
            name: "signal",
            source: include_str!("python/signal.py"),
            is_package: false,
        },
        // `keyword` — verbatim CPython keyword/soft-keyword lists +
        // membership predicates. Imported by `dataclasses` (field-name
        // validation) and `pydoc`/`inspect`-adjacent code.
        FrozenSource {
            name: "keyword",
            source: include_str!("python/keyword.py"),
            is_package: false,
        },
        // `shlex` — verbatim CPython lexical analyzer (`split`/`quote`/`join`).
        // Pure-Python over `os`/`re`/`sys`/`collections.deque`/`io.StringIO`,
        // all of which WeavePy provides. Without it pandas' `tests/io/conftest.py`
        // dies at its first line (`import shlex`) and `_load_conftests` silently
        // swallows the `ModuleNotFoundError`, dropping every io-conftest fixture
        // (`compression_to_extension`, `tips_file`, the s3/moto fixtures, …) so
        // dozens of io tests fail with a spurious "missing positional argument".
        FrozenSource {
            name: "shlex",
            source: include_str!("python/shlex.py"),
            is_package: false,
        },
        // `random` — verbatim CPython distribution layer over the
        // Rust `_random` MT19937 core (RFC 0037: `random.Random(42)`
        // is stream-identical to CPython).
        FrozenSource {
            name: "random",
            source: include_str!("python/random_mod.py"),
            is_package: false,
        },
        // `secrets` — verbatim CPython `Lib/secrets.py` (PEP 506). A
        // thin composition of `SystemRandom` + `hmac.compare_digest`,
        // both of which WeavePy already provides; the previous native
        // stub lacked `DEFAULT_ENTROPY`/`SystemRandom` and its
        // `compare_digest` skipped the str/bytes type checks
        // (test_secrets).
        FrozenSource {
            name: "secrets",
            source: include_str!("python/secrets.py"),
            is_package: false,
        },
        // `rlcompleter` — verbatim CPython source. Pure attribute/name
        // completion over `__main__` namespaces; readline is optional
        // (it degrades to import-less mode, which is exactly how the
        // suite exercises it).
        FrozenSource {
            name: "rlcompleter",
            source: include_str!("python/rlcompleter.py"),
            is_package: false,
        },
        // `uuid` — verbatim CPython `Lib/uuid.py`. The full `UUID` class
        // (immutable, `__slots__`-backed, `object.__setattr__` bypass,
        // `__str__`/`__repr__`/`__hash__`/`__eq__`, the `bytes`/`hex`/`urn`/
        // `version`/`fields` properties) is required by real code — pandas'
        // `_testing.ensure_clean()` builds temp filenames from
        // `str(uuid.uuid4())`, so a dict masquerading as a UUID produced a
        // dict-repr filename (`{'bytes': …}`) and `ENAMETOOLONG`. `uuid4()`
        // needs only `os.urandom`; the optional `_uuid` C ext is guarded by
        // `try/except ImportError`, and `getnode()`'s subprocess helpers are
        // never reached by the random/hash-based generators.
        FrozenSource {
            name: "uuid",
            source: include_str!("python/uuid.py"),
            is_package: false,
        },
        // Internal: `_SeqIter`, the lazy legacy-`__getitem__` iterator
        // `iter(obj)` returns when *obj* has no `__iter__` (CPython's
        // built-in `iterator`/seqiterobject). Kept out of `builtins` to
        // avoid leaking a name into every module's global namespace.
        FrozenSource {
            name: "_seqtools",
            source: include_str!("python/_seqtools.py"),
            is_package: false,
        },
        // `_queue` — the C accelerator surface (`Empty`, `SimpleQueue`)
        // as a Python stand-in; `queue.py` imports it eagerly and
        // test_types.test_method_descriptor_crash imports it directly.
        FrozenSource {
            name: "_queue",
            source: include_str!("python/_queue.py"),
            is_package: false,
        },
        // `collections` is the verbatim CPython package init; the
        // `_collections` accelerator below supplies `deque`/`defaultdict`
        // (which have no pure-Python fallback in the real module), while
        // `OrderedDict`/`namedtuple` run the reference pure-Python paths.
        // The verbatim CPython `_collections_abc` carries the ABC
        // definitions and `collections.abc` re-exports them (RFC 0037 WS8).
        FrozenSource {
            name: "collections",
            source: include_str!("python/collections.py"),
            is_package: true,
        },
        FrozenSource {
            name: "_collections",
            source: include_str!("python/_collections.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_collections_abc",
            source: include_str!("python/_collections_abc.py"),
            is_package: false,
        },
        // `_weakrefset` (verbatim CPython): the `WeakSet` source module
        // that `abc`/`_py_abc` import directly to back the ABC virtual-
        // subclass registry/caches (RFC 0037 WS8).
        FrozenSource {
            name: "_weakrefset",
            source: include_str!("python/_weakrefset.py"),
            is_package: false,
        },
        // `_py_abc` (verbatim CPython): the pure-Python `ABCMeta`
        // reference implementation. `test_abc` imports it directly to
        // exercise the Python ABC machinery alongside the C `_abc` path.
        FrozenSource {
            name: "_py_abc",
            source: include_str!("python/_py_abc.py"),
            is_package: false,
        },
        // `_colorize`: CPython 3.13's ANSI-colour helper (verbatim). Imported
        // by `traceback`/`test_traceback` (and the 3.13 REPL); honours
        // NO_COLOR/FORCE_COLOR and TTY detection.
        FrozenSource {
            name: "_colorize",
            source: include_str!("python/_colorize.py"),
            is_package: false,
        },
        // `__future__`: the feature-flag table (verbatim CPython 3.13).
        // `from __future__ import annotations` is a compiler directive, but
        // the module must still be importable because real modules read its
        // `_Feature` objects (e.g. `__future__.annotations`).
        FrozenSource {
            name: "__future__",
            source: include_str!("python/future_module.py"),
            is_package: false,
        },
        FrozenSource {
            name: "collections.abc",
            source: include_str!("python/collections_abc.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_collections_user",
            source: include_str!("python/_collections_user.py"),
            is_package: false,
        },
        // RFC 0036 — `string` (constants + `Template` + `Formatter` over
        // the native `_string`) and `platform`, carried verbatim from
        // CPython 3.13. RFC 0076 WS15 turns `string` into a package
        // (CPython 3.14 layout) so the PEP 750 `string.templatelib`
        // submodule is importable; the `__init__` body is unchanged.
        FrozenSource {
            name: "string",
            source: include_str!("python/string/__init__.py"),
            is_package: true,
        },
        FrozenSource {
            name: "string.templatelib",
            source: include_str!("python/string/templatelib.py"),
            is_package: false,
        },
        // `base64` is CPython's `Lib/base64.py` ported verbatim (pure Python
        // over `binascii` + `struct` + `re`). It supersedes the old Rust
        // `base64` module, which covered only RFC 3548 and ignored
        // `altchars`/`validate`; the frozen copy adds a85/b85/z85 and the
        // exact decode semantics `test_base64` checks.
        FrozenSource {
            name: "base64",
            source: include_str!("python/base64_mod.py"),
            is_package: false,
        },
        FrozenSource {
            name: "platform",
            source: include_str!("python/platform.py"),
            is_package: false,
        },
        // Verbatim CPython 3.13 `hmac`. The Rust shim it replaces could not
        // satisfy `test_hmac`'s identity check (`hmac.compare_digest is
        // _operator._compare_digest`) nor the full `HMAC` class surface;
        // ported over `hashlib` + `_operator._compare_digest` instead.
        FrozenSource {
            name: "hmac",
            source: include_str!("python/hmac.py"),
            is_package: false,
        },
        FrozenSource {
            name: "itertools",
            source: include_str!("python/itertools.py"),
            is_package: false,
        },
        FrozenSource {
            name: "functools",
            source: include_str!("python/functools.py"),
            is_package: false,
        },
        // RFC 0041 WS-json — the verbatim CPython `json` package. Each
        // submodule prefers the native `_json` accelerator and falls back to
        // its pure-Python twin, so blocking `_json` (the way `test_json`
        // probes for the C build) transparently selects the Python path.
        FrozenSource {
            name: "json",
            source: include_str!("python/json/__init__.py"),
            is_package: true,
        },
        FrozenSource {
            name: "json.decoder",
            source: include_str!("python/json/decoder.py"),
            is_package: false,
        },
        FrozenSource {
            name: "json.encoder",
            source: include_str!("python/json/encoder.py"),
            is_package: false,
        },
        FrozenSource {
            name: "json.scanner",
            source: include_str!("python/json/scanner.py"),
            is_package: false,
        },
        FrozenSource {
            name: "json.tool",
            source: include_str!("python/json/tool.py"),
            is_package: false,
        },
        // RFC 0037 WS8 verbatim/faithful module ports that gate import-time
        // clusters: the C-locale `locale` unblocks `test_format`
        // and backs `calendar`'s `LocaleTextCalendar`; `calendar` is the
        // verbatim CPython 3.13 module. (`cmath` is now a native module —
        // see stdlib/cmath_mod.rs.)
        FrozenSource {
            name: "locale",
            source: include_str!("python/locale.py"),
            is_package: false,
        },
        FrozenSource {
            name: "calendar",
            source: include_str!("python/calendar.py"),
            is_package: false,
        },
        // RFC 0040 WS8 — `time.strptime` delegates here, exactly as
        // CPython's `timemodule.c` does (`_strptime._strptime_time`).
        FrozenSource {
            name: "_strptime",
            source: include_str!("python/_strptime.py"),
            is_package: false,
        },
        FrozenSource {
            name: "contextlib",
            source: include_str!("python/contextlib.py"),
            is_package: false,
        },
        // `pathlib` is CPython 3.13's verbatim package: the thin `__init__`
        // re-exports `_abc` (the `PurePathBase`/`PathBase` ABCs the
        // `test_pathlib_abc` suite drives) and `_local` (the concrete
        // `PurePath`/`Path`/`PurePosixPath`/`PosixPath`/… classes). Ported
        // wholesale rather than re-approximated (RFC 0038 WS-B).
        FrozenSource {
            name: "pathlib",
            source: include_str!("python/pathlib.py"),
            is_package: true,
        },
        FrozenSource {
            name: "pathlib._local",
            source: include_str!("python/pathlib_local.py"),
            is_package: false,
        },
        FrozenSource {
            name: "argparse",
            source: include_str!("python/argparse.py"),
            is_package: false,
        },
        FrozenSource {
            name: "abc",
            source: include_str!("python/abc.py"),
            is_package: false,
        },
        FrozenSource {
            name: "enum",
            source: include_str!("python/enum.py"),
            is_package: false,
        },
        // `tty` + `pty` verbatim from CPython — they run over the native
        // `termios` builtin (RFC 0055 WS6: real terminal control, so the
        // pty-backed legs of `test_asyncio.test_events`, `test_termios`,
        // `test_tty`, and `test_ioctl` measure the real syscalls).
        FrozenSource {
            name: "tty",
            source: include_str!("python/tty.py"),
            is_package: false,
        },
        FrozenSource {
            name: "pty",
            source: include_str!("python/pty.py"),
            is_package: false,
        },
        FrozenSource {
            name: "dataclasses",
            source: include_str!("python/dataclasses.py"),
            is_package: false,
        },
        // RFC 0051: CPython's verbatim `Lib/typing.py` over the
        // pure-Python `_typing` support module (the C accelerator
        // surface from `Objects/typevarobject.c`, re-implemented).
        FrozenSource {
            name: "typing",
            source: include_str!("python/typing.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_typing",
            source: include_str!("python/_typing.py"),
            is_package: false,
        },
        FrozenSource {
            name: "heapq",
            source: include_str!("python/heapq.py"),
            is_package: false,
        },
        FrozenSource {
            name: "threading",
            source: include_str!("python/threading.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_threading_local",
            source: include_str!("python/_threading_local.py"),
            is_package: false,
        },
        FrozenSource {
            name: "queue",
            source: include_str!("python/queue.py"),
            is_package: false,
        },
        // RFC 0040 (WS5): the *real* CPython `multiprocessing` package,
        // frozen verbatim from `vendor/cpython/Lib/multiprocessing/`,
        // running over the native `_multiprocessing` SemLock core, the
        // `_posixshmem` shared-memory core, `_posixsubprocess.fork_exec`
        // (spawn rides the standard `weavepy -c ...` + `os.posix_spawn`
        // path), and `os.fork` (the fork start method). Replaces the
        // single-file RFC 0026 shim. The Windows-only submodules
        // (`popen_spawn_win32`) are frozen for completeness but never
        // imported on POSIX.
        FrozenSource {
            name: "multiprocessing",
            source: include_str!("python/multiprocessing/__init__.py"),
            is_package: true,
        },
        FrozenSource {
            name: "multiprocessing.connection",
            source: include_str!("python/multiprocessing/connection.py"),
            is_package: false,
        },
        FrozenSource {
            name: "multiprocessing.context",
            source: include_str!("python/multiprocessing/context.py"),
            is_package: false,
        },
        FrozenSource {
            name: "multiprocessing.forkserver",
            source: include_str!("python/multiprocessing/forkserver.py"),
            is_package: false,
        },
        FrozenSource {
            name: "multiprocessing.heap",
            source: include_str!("python/multiprocessing/heap.py"),
            is_package: false,
        },
        FrozenSource {
            name: "multiprocessing.managers",
            source: include_str!("python/multiprocessing/managers.py"),
            is_package: false,
        },
        FrozenSource {
            name: "multiprocessing.pool",
            source: include_str!("python/multiprocessing/pool.py"),
            is_package: false,
        },
        FrozenSource {
            name: "multiprocessing.popen_fork",
            source: include_str!("python/multiprocessing/popen_fork.py"),
            is_package: false,
        },
        FrozenSource {
            name: "multiprocessing.popen_forkserver",
            source: include_str!("python/multiprocessing/popen_forkserver.py"),
            is_package: false,
        },
        FrozenSource {
            name: "multiprocessing.popen_spawn_posix",
            source: include_str!("python/multiprocessing/popen_spawn_posix.py"),
            is_package: false,
        },
        FrozenSource {
            name: "multiprocessing.popen_spawn_win32",
            source: include_str!("python/multiprocessing/popen_spawn_win32.py"),
            is_package: false,
        },
        FrozenSource {
            name: "multiprocessing.process",
            source: include_str!("python/multiprocessing/process.py"),
            is_package: false,
        },
        FrozenSource {
            name: "multiprocessing.queues",
            source: include_str!("python/multiprocessing/queues.py"),
            is_package: false,
        },
        FrozenSource {
            name: "multiprocessing.reduction",
            source: include_str!("python/multiprocessing/reduction.py"),
            is_package: false,
        },
        FrozenSource {
            name: "multiprocessing.resource_sharer",
            source: include_str!("python/multiprocessing/resource_sharer.py"),
            is_package: false,
        },
        FrozenSource {
            name: "multiprocessing.resource_tracker",
            source: include_str!("python/multiprocessing/resource_tracker.py"),
            is_package: false,
        },
        FrozenSource {
            name: "multiprocessing.shared_memory",
            source: include_str!("python/multiprocessing/shared_memory.py"),
            is_package: false,
        },
        FrozenSource {
            name: "multiprocessing.sharedctypes",
            source: include_str!("python/multiprocessing/sharedctypes.py"),
            is_package: false,
        },
        FrozenSource {
            name: "multiprocessing.spawn",
            source: include_str!("python/multiprocessing/spawn.py"),
            is_package: false,
        },
        FrozenSource {
            name: "multiprocessing.synchronize",
            source: include_str!("python/multiprocessing/synchronize.py"),
            is_package: false,
        },
        FrozenSource {
            name: "multiprocessing.util",
            source: include_str!("python/multiprocessing/util.py"),
            is_package: false,
        },
        FrozenSource {
            name: "multiprocessing.dummy",
            source: include_str!("python/multiprocessing/dummy/__init__.py"),
            is_package: true,
        },
        FrozenSource {
            name: "multiprocessing.dummy.connection",
            source: include_str!("python/multiprocessing/dummy/connection.py"),
            is_package: false,
        },
        // RFC 0039 (WS7): the *real* CPython `concurrent.futures`
        // package, frozen verbatim from `vendor/cpython/Lib/concurrent/
        // futures/`. `ThreadPoolExecutor` now spins up real OS worker
        // threads (the old single-file shim ran `submit`ted work
        // synchronously on the caller, which broke `run_in_executor`
        // thread-affinity and the `test_asyncio` executor tests). The
        // dotted names resolve via the registered module name, not the
        // source filename. `process` is a stub (no multiprocessing
        // runtime); it stays importable so the lazy `__getattr__` in
        // `__init__` and `from concurrent.futures import *` still work.
        FrozenSource {
            name: "concurrent",
            source: "",
            is_package: true,
        },
        FrozenSource {
            name: "concurrent.futures",
            source: include_str!("python/concurrent_futures_init.py"),
            is_package: true,
        },
        FrozenSource {
            name: "concurrent.futures._base",
            source: include_str!("python/concurrent_futures_base.py"),
            is_package: false,
        },
        FrozenSource {
            name: "concurrent.futures.thread",
            source: include_str!("python/concurrent_futures_thread.py"),
            is_package: false,
        },
        FrozenSource {
            name: "concurrent.futures.process",
            source: include_str!("python/concurrent_futures_process.py"),
            is_package: false,
        },
        // RFC 0039 (WS7): the *real* CPython `asyncio` package, frozen
        // verbatim from `vendor/cpython/Lib/asyncio/`, running over the WS6
        // native selector backends. Replaces the old cooperative single-file
        // shim. The Windows-only submodules (`windows_events`/`windows_utils`/
        // `proactor_events`) are frozen for completeness but never imported on
        // a non-win32 build, so their `_winapi`/`_overlapped` deps don't load.
        FrozenSource {
            name: "asyncio",
            source: include_str!("python/asyncio/__init__.py"),
            is_package: true,
        },
        FrozenSource {
            name: "asyncio.base_events",
            source: include_str!("python/asyncio/base_events.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.base_futures",
            source: include_str!("python/asyncio/base_futures.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.base_subprocess",
            source: include_str!("python/asyncio/base_subprocess.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.base_tasks",
            source: include_str!("python/asyncio/base_tasks.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.constants",
            source: include_str!("python/asyncio/constants.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.coroutines",
            source: include_str!("python/asyncio/coroutines.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.events",
            source: include_str!("python/asyncio/events.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.exceptions",
            source: include_str!("python/asyncio/exceptions.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.format_helpers",
            source: include_str!("python/asyncio/format_helpers.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.futures",
            source: include_str!("python/asyncio/futures.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.locks",
            source: include_str!("python/asyncio/locks.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.log",
            source: include_str!("python/asyncio/log.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.mixins",
            source: include_str!("python/asyncio/mixins.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.proactor_events",
            source: include_str!("python/asyncio/proactor_events.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.protocols",
            source: include_str!("python/asyncio/protocols.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.queues",
            source: include_str!("python/asyncio/queues.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.runners",
            source: include_str!("python/asyncio/runners.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.selector_events",
            source: include_str!("python/asyncio/selector_events.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.sslproto",
            source: include_str!("python/asyncio/sslproto.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.staggered",
            source: include_str!("python/asyncio/staggered.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.streams",
            source: include_str!("python/asyncio/streams.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.subprocess",
            source: include_str!("python/asyncio/subprocess.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.taskgroups",
            source: include_str!("python/asyncio/taskgroups.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.tasks",
            source: include_str!("python/asyncio/tasks.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.threads",
            source: include_str!("python/asyncio/threads.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.timeouts",
            source: include_str!("python/asyncio/timeouts.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.transports",
            source: include_str!("python/asyncio/transports.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.trsock",
            source: include_str!("python/asyncio/trsock.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.unix_events",
            source: include_str!("python/asyncio/unix_events.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.windows_events",
            source: include_str!("python/asyncio/windows_events.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.windows_utils",
            source: include_str!("python/asyncio/windows_utils.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.__main__",
            source: include_str!("python/asyncio/__main__.py"),
            is_package: false,
        },
        // High-level wrappers over Rust cores from RFC 0017.
        FrozenSource {
            name: "subprocess",
            source: include_str!("python/subprocess.py"),
            is_package: false,
        },
        FrozenSource {
            name: "socket",
            source: include_str!("python/socket.py"),
            is_package: false,
        },
        // RFC 0042 WS2 — CPython-shaped `ssl` over the native rustls `_ssl`
        // core (mirrors CPython's `Lib/ssl.py` over its `_ssl` C extension).
        FrozenSource {
            name: "ssl",
            source: include_str!("python/ssl.py"),
            is_package: false,
        },
        FrozenSource {
            name: "selectors",
            source: include_str!("python/selectors.py"),
            is_package: false,
        },
        FrozenSource {
            name: "tempfile",
            source: include_str!("python/tempfile.py"),
            is_package: false,
        },
        FrozenSource {
            name: "shutil",
            source: include_str!("python/shutil.py"),
            is_package: false,
        },
        // `fnmatch` / `glob` — verbatim CPython 3.13 ports (replacing the
        // earlier Rust shims). `glob` exposes the `_Globber`/`_StringGlobber`
        // helpers that the 3.13 `pathlib` rewrite imports.
        FrozenSource {
            name: "fnmatch",
            source: include_str!("python/fnmatch.py"),
            is_package: false,
        },
        FrozenSource {
            name: "glob",
            source: include_str!("python/glob.py"),
            is_package: false,
        },
        FrozenSource {
            name: "csv",
            source: include_str!("python/csv.py"),
            is_package: false,
        },
        FrozenSource {
            name: "mimetypes",
            source: include_str!("python/mimetypes.py"),
            is_package: false,
        },
        FrozenSource {
            name: "ipaddress",
            source: include_str!("python/ipaddress.py"),
            is_package: false,
        },
        FrozenSource {
            name: "socketserver",
            source: include_str!("python/socketserver.py"),
            is_package: false,
        },
        FrozenSource {
            name: "html",
            source: include_str!("python/html.py"),
            is_package: false,
        },
        // `html.parser` + `_markupbase` — verbatim CPython (RFC 0056 WS3):
        // the earlier 134-line regex shim mis-parsed CDATA/declaration/bogus
        // -comment paths and looped on truncated markup, failing (and
        // hanging) test_htmlparser.
        FrozenSource {
            name: "html.parser",
            source: include_str!("python/html_parser.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_markupbase",
            source: include_str!("python/_markupbase.py"),
            is_package: false,
        },
        FrozenSource {
            name: "html.entities",
            source: include_str!("python/html_entities.py"),
            is_package: false,
        },
        // RFC 0042 WS4 — `urllib`, vendored verbatim from
        // `vendor/cpython/Lib/urllib/` (the `__init__` is empty upstream).
        // `request`/`response`/`error` now ride the WS1 `socket.makefile()`
        // and WS2 `ssl` stacks; `parse` was already verbatim.
        FrozenSource {
            name: "urllib",
            source: "",
            is_package: true,
        },
        FrozenSource {
            name: "urllib.parse",
            source: include_str!("python/urllib/parse.py"),
            is_package: false,
        },
        FrozenSource {
            name: "urllib.error",
            source: include_str!("python/urllib/error.py"),
            is_package: false,
        },
        FrozenSource {
            name: "urllib.response",
            source: include_str!("python/urllib/response.py"),
            is_package: false,
        },
        FrozenSource {
            name: "urllib.request",
            source: include_str!("python/urllib/request.py"),
            is_package: false,
        },
        FrozenSource {
            name: "urllib.robotparser",
            source: include_str!("python/urllib/robotparser.py"),
            is_package: false,
        },
        // RFC 0042 WS3 — `http`, vendored verbatim from
        // `vendor/cpython/Lib/http/`. The real `__init__` exports the
        // `HTTPStatus`/`HTTPMethod` enums; `client`/`server` run over the WS1
        // `socket.makefile()` + WS2 `ssl` stacks. `cookiejar` (WS4) lets
        // `urllib.request.HTTPCookieProcessor` work unchanged.
        FrozenSource {
            name: "http",
            source: include_str!("python/http/__init__.py"),
            is_package: true,
        },
        FrozenSource {
            name: "http.client",
            source: include_str!("python/http/client.py"),
            is_package: false,
        },
        FrozenSource {
            name: "http.server",
            source: include_str!("python/http/server.py"),
            is_package: false,
        },
        FrozenSource {
            name: "http.cookies",
            source: include_str!("python/http/cookies.py"),
            is_package: false,
        },
        FrozenSource {
            name: "http.cookiejar",
            source: include_str!("python/http/cookiejar.py"),
            is_package: false,
        },
        // RFC 0056 WS6 — `wsgiref`, vendored verbatim from
        // `vendor/cpython/Lib/wsgiref/`. Django's test client imports
        // `wsgiref.simple_server` (via `django.core.servers.basehttp`),
        // and flask's dev server sits on `wsgiref.types` through werkzeug.
        FrozenSource {
            name: "wsgiref",
            source: include_str!("python/wsgiref/__init__.py"),
            is_package: true,
        },
        FrozenSource {
            name: "wsgiref.handlers",
            source: include_str!("python/wsgiref/handlers.py"),
            is_package: false,
        },
        FrozenSource {
            name: "wsgiref.headers",
            source: include_str!("python/wsgiref/headers.py"),
            is_package: false,
        },
        FrozenSource {
            name: "wsgiref.simple_server",
            source: include_str!("python/wsgiref/simple_server.py"),
            is_package: false,
        },
        FrozenSource {
            name: "wsgiref.types",
            source: include_str!("python/wsgiref/types.py"),
            is_package: false,
        },
        FrozenSource {
            name: "wsgiref.util",
            source: include_str!("python/wsgiref/util.py"),
            is_package: false,
        },
        FrozenSource {
            name: "wsgiref.validate",
            source: include_str!("python/wsgiref/validate.py"),
            is_package: false,
        },
        // RFC 0042 WS3/WS5 — the real CPython `email` package, vendored
        // verbatim from `vendor/cpython/Lib/email/`. `http.client` parses
        // response headers with `email.parser`/`email.message`, and the
        // WS5 mail clients (`smtplib` etc.) build messages with `email.mime`.
        FrozenSource {
            name: "email",
            source: include_str!("python/email/__init__.py"),
            is_package: true,
        },
        FrozenSource {
            name: "email._encoded_words",
            source: include_str!("python/email/_encoded_words.py"),
            is_package: false,
        },
        FrozenSource {
            name: "email._header_value_parser",
            source: include_str!("python/email/_header_value_parser.py"),
            is_package: false,
        },
        FrozenSource {
            name: "email._parseaddr",
            source: include_str!("python/email/_parseaddr.py"),
            is_package: false,
        },
        FrozenSource {
            name: "email._policybase",
            source: include_str!("python/email/_policybase.py"),
            is_package: false,
        },
        FrozenSource {
            name: "email.base64mime",
            source: include_str!("python/email/base64mime.py"),
            is_package: false,
        },
        FrozenSource {
            name: "email.charset",
            source: include_str!("python/email/charset.py"),
            is_package: false,
        },
        FrozenSource {
            name: "email.contentmanager",
            source: include_str!("python/email/contentmanager.py"),
            is_package: false,
        },
        FrozenSource {
            name: "email.encoders",
            source: include_str!("python/email/encoders.py"),
            is_package: false,
        },
        FrozenSource {
            name: "email.errors",
            source: include_str!("python/email/errors.py"),
            is_package: false,
        },
        FrozenSource {
            name: "email.feedparser",
            source: include_str!("python/email/feedparser.py"),
            is_package: false,
        },
        FrozenSource {
            name: "email.generator",
            source: include_str!("python/email/generator.py"),
            is_package: false,
        },
        FrozenSource {
            name: "email.header",
            source: include_str!("python/email/header.py"),
            is_package: false,
        },
        FrozenSource {
            name: "email.headerregistry",
            source: include_str!("python/email/headerregistry.py"),
            is_package: false,
        },
        FrozenSource {
            name: "email.iterators",
            source: include_str!("python/email/iterators.py"),
            is_package: false,
        },
        FrozenSource {
            name: "email.message",
            source: include_str!("python/email/message.py"),
            is_package: false,
        },
        FrozenSource {
            name: "email.parser",
            source: include_str!("python/email/parser.py"),
            is_package: false,
        },
        FrozenSource {
            name: "email.policy",
            source: include_str!("python/email/policy.py"),
            is_package: false,
        },
        FrozenSource {
            name: "email.quoprimime",
            source: include_str!("python/email/quoprimime.py"),
            is_package: false,
        },
        FrozenSource {
            name: "email.utils",
            source: include_str!("python/email/utils.py"),
            is_package: false,
        },
        // `email.mime.*` — message construction helpers (WS5 mail clients).
        FrozenSource {
            name: "email.mime",
            source: include_str!("python/email/mime/__init__.py"),
            is_package: true,
        },
        FrozenSource {
            name: "email.mime.application",
            source: include_str!("python/email/mime/application.py"),
            is_package: false,
        },
        FrozenSource {
            name: "email.mime.audio",
            source: include_str!("python/email/mime/audio.py"),
            is_package: false,
        },
        FrozenSource {
            name: "email.mime.base",
            source: include_str!("python/email/mime/base.py"),
            is_package: false,
        },
        FrozenSource {
            name: "email.mime.image",
            source: include_str!("python/email/mime/image.py"),
            is_package: false,
        },
        FrozenSource {
            name: "email.mime.message",
            source: include_str!("python/email/mime/message.py"),
            is_package: false,
        },
        FrozenSource {
            name: "email.mime.multipart",
            source: include_str!("python/email/mime/multipart.py"),
            is_package: false,
        },
        FrozenSource {
            name: "email.mime.nonmultipart",
            source: include_str!("python/email/mime/nonmultipart.py"),
            is_package: false,
        },
        FrozenSource {
            name: "email.mime.text",
            source: include_str!("python/email/mime/text.py"),
            is_package: false,
        },
        // `quopri` — quoted-printable codec used by `email`'s encoders/parsers
        // (verbatim CPython, over the native `binascii` a2b_qp/b2a_qp).
        FrozenSource {
            name: "quopri",
            source: include_str!("python/quopri.py"),
            is_package: false,
        },
        // `_scproxy` — macOS system-proxy shim (reports "no system proxy"); the
        // verbatim `urllib.request` imports it unconditionally on darwin.
        FrozenSource {
            name: "_scproxy",
            source: include_str!("python/_scproxy.py"),
            is_package: false,
        },
        // `stringprep` (RFC 3454 tables) + the `encodings.idna`/`encodings.punycode`
        // codecs. WeavePy serves most codecs natively, but `idna`/`punycode` are
        // pure-Python in CPython and are resolved on demand by `codecs.lookup`
        // (see `python/codecs.py`). `http.client`/`urllib` need `idna` to encode
        // non-ASCII hostnames. The `encodings` package is intentionally minimal
        // (just these two modules); it is NOT the codec search bootstrap.
        FrozenSource {
            name: "stringprep",
            source: include_str!("python/stringprep.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings",
            source: include_str!("python/encodings/__init__.py"),
            is_package: true,
        },
        FrozenSource {
            name: "encodings.idna",
            source: include_str!("python/encodings/idna.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.punycode",
            source: include_str!("python/encodings/punycode.py"),
            is_package: false,
        },
        // CPython's alias registry (`encodings/aliases.py`, vendored verbatim).
        // `codecs.lookup` resolves spelling variants through it exactly like
        // CPython's `encodings.search_function` — e.g. `utf-16le` → `utf_16_le`,
        // `windows-1251` → `cp1251` — before consulting the codec tables.
        FrozenSource {
            name: "encodings.aliases",
            source: include_str!("python/encodings/aliases.py"),
            is_package: false,
        },
        // On-demand single-byte codepages CPython ships as `gencodec.py`-
        // generated `encodings.*` charmap modules (`codecs.charmap_encode`/
        // `charmap_decode`). `encoding_rs` (the native backend) carries only
        // the WHATWG set, so these EBCDIC/DOS pages are missing there and
        // resolve through `codecs.lookup`'s frozen-`encodings` fallback:
        // `cp037` (pandas `read_csv`/`to_csv` non-UTF-8 tests) and `cp737`
        // (Greek DOS, `Series.to_csv` compression matrix).
        FrozenSource {
            name: "encodings.cp037",
            source: include_str!("python/encodings/cp037.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.cp737",
            source: include_str!("python/encodings/cp737.py"),
            is_package: false,
        },
        // RFC 0077: the rest of CPython 3.14's `encodings` package, vendored
        // verbatim so every `encodings.aliases` target is an importable module
        // (test_codecs.test_alias_modules_exist) with CPython's class surface.
        // The UTF/escape/transform modules run directly on `_codecs`; the CJK
        // modules build on `_multibytecodec` + the `_codecs_{cn,hk,iso2022,
        // jp,kr,tw}.getcodec` shims below, which hand them handles onto the
        // frozen Python codec implementations. `codecs.lookup` keeps serving
        // its own CodecInfos for the names it special-cases.
        FrozenSource {
            name: "encodings.big5",
            source: include_str!("python/encodings/big5.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.big5hkscs",
            source: include_str!("python/encodings/big5hkscs.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.bz2_codec",
            source: include_str!("python/encodings/bz2_codec.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.cp932",
            source: include_str!("python/encodings/cp932.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.cp949",
            source: include_str!("python/encodings/cp949.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.cp950",
            source: include_str!("python/encodings/cp950.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.euc_jis_2004",
            source: include_str!("python/encodings/euc_jis_2004.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.euc_jisx0213",
            source: include_str!("python/encodings/euc_jisx0213.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.euc_jp",
            source: include_str!("python/encodings/euc_jp.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.euc_kr",
            source: include_str!("python/encodings/euc_kr.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.gb18030",
            source: include_str!("python/encodings/gb18030.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.gb2312",
            source: include_str!("python/encodings/gb2312.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.gbk",
            source: include_str!("python/encodings/gbk.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.hex_codec",
            source: include_str!("python/encodings/hex_codec.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.hz",
            source: include_str!("python/encodings/hz.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.iso2022_jp",
            source: include_str!("python/encodings/iso2022_jp.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.iso2022_jp_1",
            source: include_str!("python/encodings/iso2022_jp_1.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.iso2022_jp_2",
            source: include_str!("python/encodings/iso2022_jp_2.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.iso2022_jp_2004",
            source: include_str!("python/encodings/iso2022_jp_2004.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.iso2022_jp_3",
            source: include_str!("python/encodings/iso2022_jp_3.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.iso2022_jp_ext",
            source: include_str!("python/encodings/iso2022_jp_ext.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.iso2022_kr",
            source: include_str!("python/encodings/iso2022_kr.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.johab",
            source: include_str!("python/encodings/johab.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.quopri_codec",
            source: include_str!("python/encodings/quopri_codec.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.raw_unicode_escape",
            source: include_str!("python/encodings/raw_unicode_escape.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.shift_jis",
            source: include_str!("python/encodings/shift_jis.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.shift_jis_2004",
            source: include_str!("python/encodings/shift_jis_2004.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.shift_jisx0213",
            source: include_str!("python/encodings/shift_jisx0213.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.unicode_escape",
            source: include_str!("python/encodings/unicode_escape.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.utf_16",
            source: include_str!("python/encodings/utf_16.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.utf_16_be",
            source: include_str!("python/encodings/utf_16_be.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.utf_16_le",
            source: include_str!("python/encodings/utf_16_le.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.utf_32",
            source: include_str!("python/encodings/utf_32.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.utf_32_be",
            source: include_str!("python/encodings/utf_32_be.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.utf_32_le",
            source: include_str!("python/encodings/utf_32_le.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.utf_7",
            source: include_str!("python/encodings/utf_7.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.utf_8_sig",
            source: include_str!("python/encodings/utf_8_sig.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.uu_codec",
            source: include_str!("python/encodings/uu_codec.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.zlib_codec",
            source: include_str!("python/encodings/zlib_codec.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings._win_cp_codecs",
            source: include_str!("python/encodings/_win_cp_codecs.py"),
            is_package: false,
        },
        // Vendored verbatim from CPython 3.13 for code that reaches into the
        // `encodings` package directly (`encodings.ascii.StreamReader`,
        // `from encodings.rot_13 import rot13`, the always-raising
        // `undefined` codec).
        FrozenSource {
            name: "encodings.ascii",
            source: include_str!("python/encodings/ascii.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.utf_8",
            source: include_str!("python/encodings/utf_8.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.rot_13",
            source: include_str!("python/encodings/rot_13.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.undefined",
            source: include_str!("python/encodings/undefined.py"),
            is_package: false,
        },
        // RFC 0063 — the ANSI/OEM code-page codecs. CPython ships these
        // unconditionally; on non-Windows the `from codecs import
        // mbcs_encode …` line raises ImportError and `codecs.lookup`
        // treats the module as a miss (exactly CPython's behaviour).
        FrozenSource {
            name: "encodings.mbcs",
            source: include_str!("python/encodings/mbcs.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.oem",
            source: include_str!("python/encodings/oem.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.base64_codec",
            source: include_str!("python/encodings/base64_codec.py"),
            is_package: false,
        },
        // gencodec.py charmap codepages vendored verbatim from CPython 3.13;
        // they ride the frozen `codecs.charmap_encode`/`charmap_decode`.
        FrozenSource {
            name: "encodings.cp856",
            source: include_str!("python/encodings/cp856.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.cp874",
            source: include_str!("python/encodings/cp874.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.cp875",
            source: include_str!("python/encodings/cp875.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.cp1006",
            source: include_str!("python/encodings/cp1006.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.cp1125",
            source: include_str!("python/encodings/cp1125.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.cp1140",
            source: include_str!("python/encodings/cp1140.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.koi8_t",
            source: include_str!("python/encodings/koi8_t.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.kz1048",
            source: include_str!("python/encodings/kz1048.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.mac_arabic",
            source: include_str!("python/encodings/mac_arabic.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.palmos",
            source: include_str!("python/encodings/palmos.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.ptcp154",
            source: include_str!("python/encodings/ptcp154.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.tis_620",
            source: include_str!("python/encodings/tis_620.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.charmap",
            source: include_str!("python/encodings/charmap.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.latin_1",
            source: include_str!("python/encodings/latin_1.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.cp1026",
            source: include_str!("python/encodings/cp1026.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.cp1250",
            source: include_str!("python/encodings/cp1250.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.cp1251",
            source: include_str!("python/encodings/cp1251.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.cp1252",
            source: include_str!("python/encodings/cp1252.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.cp1253",
            source: include_str!("python/encodings/cp1253.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.cp1254",
            source: include_str!("python/encodings/cp1254.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.cp1255",
            source: include_str!("python/encodings/cp1255.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.cp1256",
            source: include_str!("python/encodings/cp1256.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.cp1257",
            source: include_str!("python/encodings/cp1257.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.cp1258",
            source: include_str!("python/encodings/cp1258.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.cp273",
            source: include_str!("python/encodings/cp273.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.cp424",
            source: include_str!("python/encodings/cp424.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.cp437",
            source: include_str!("python/encodings/cp437.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.cp500",
            source: include_str!("python/encodings/cp500.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.cp720",
            source: include_str!("python/encodings/cp720.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.cp775",
            source: include_str!("python/encodings/cp775.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.cp850",
            source: include_str!("python/encodings/cp850.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.cp852",
            source: include_str!("python/encodings/cp852.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.cp855",
            source: include_str!("python/encodings/cp855.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.cp857",
            source: include_str!("python/encodings/cp857.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.cp858",
            source: include_str!("python/encodings/cp858.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.cp860",
            source: include_str!("python/encodings/cp860.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.cp861",
            source: include_str!("python/encodings/cp861.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.cp862",
            source: include_str!("python/encodings/cp862.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.cp863",
            source: include_str!("python/encodings/cp863.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.cp864",
            source: include_str!("python/encodings/cp864.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.cp865",
            source: include_str!("python/encodings/cp865.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.cp866",
            source: include_str!("python/encodings/cp866.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.cp869",
            source: include_str!("python/encodings/cp869.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.hp_roman8",
            source: include_str!("python/encodings/hp_roman8.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.iso8859_1",
            source: include_str!("python/encodings/iso8859_1.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.iso8859_2",
            source: include_str!("python/encodings/iso8859_2.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.iso8859_3",
            source: include_str!("python/encodings/iso8859_3.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.iso8859_4",
            source: include_str!("python/encodings/iso8859_4.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.iso8859_5",
            source: include_str!("python/encodings/iso8859_5.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.iso8859_6",
            source: include_str!("python/encodings/iso8859_6.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.iso8859_7",
            source: include_str!("python/encodings/iso8859_7.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.iso8859_8",
            source: include_str!("python/encodings/iso8859_8.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.iso8859_9",
            source: include_str!("python/encodings/iso8859_9.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.iso8859_10",
            source: include_str!("python/encodings/iso8859_10.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.iso8859_11",
            source: include_str!("python/encodings/iso8859_11.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.iso8859_13",
            source: include_str!("python/encodings/iso8859_13.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.iso8859_14",
            source: include_str!("python/encodings/iso8859_14.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.iso8859_15",
            source: include_str!("python/encodings/iso8859_15.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.iso8859_16",
            source: include_str!("python/encodings/iso8859_16.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.koi8_r",
            source: include_str!("python/encodings/koi8_r.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.koi8_u",
            source: include_str!("python/encodings/koi8_u.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.mac_croatian",
            source: include_str!("python/encodings/mac_croatian.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.mac_cyrillic",
            source: include_str!("python/encodings/mac_cyrillic.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.mac_farsi",
            source: include_str!("python/encodings/mac_farsi.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.mac_greek",
            source: include_str!("python/encodings/mac_greek.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.mac_iceland",
            source: include_str!("python/encodings/mac_iceland.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.mac_latin2",
            source: include_str!("python/encodings/mac_latin2.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.mac_roman",
            source: include_str!("python/encodings/mac_roman.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.mac_romanian",
            source: include_str!("python/encodings/mac_romanian.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.mac_turkish",
            source: include_str!("python/encodings/mac_turkish.py"),
            is_package: false,
        },
        // RFC 0042 WS5 — application-protocol clients, vendored verbatim from
        // CPython 3.13. They ride the WS1 `socket`/`makefile()` and WS2 `ssl`
        // stacks (`*_SSL` variants, `starttls`/`stls`). `nntplib`/`telnetlib`
        // were removed upstream in 3.13, so they are intentionally absent.
        FrozenSource {
            name: "ftplib",
            source: include_str!("python/ftplib.py"),
            is_package: false,
        },
        FrozenSource {
            name: "poplib",
            source: include_str!("python/poplib.py"),
            is_package: false,
        },
        FrozenSource {
            name: "imaplib",
            source: include_str!("python/imaplib.py"),
            is_package: false,
        },
        FrozenSource {
            name: "smtplib",
            source: include_str!("python/smtplib.py"),
            is_package: false,
        },
        // `xml` package + submodules. `etree` and `dom` are now the *verbatim*
        // CPython implementations running over WeavePy's native `pyexpat`
        // (namespace-aware `ParserCreate(encoding, "}")`, `_elementtree` C
        // accelerator absent so the pure-Python path is taken). This replaces
        // the earlier hand-rolled `xml_etree.py`, which was namespace-naive and
        // failed pandas' `read_xml`/`to_xml` namespace + prefix round-trips.
        FrozenSource {
            name: "xml",
            source: "",
            is_package: true,
        },
        FrozenSource {
            name: "xml.etree",
            source: include_str!("python/xml/etree/__init__.py"),
            is_package: true,
        },
        FrozenSource {
            name: "xml.etree.ElementPath",
            source: include_str!("python/xml/etree/ElementPath.py"),
            is_package: false,
        },
        FrozenSource {
            name: "xml.etree.ElementTree",
            source: include_str!("python/xml/etree/ElementTree.py"),
            is_package: false,
        },
        FrozenSource {
            name: "xml.etree.ElementInclude",
            source: include_str!("python/xml/etree/ElementInclude.py"),
            is_package: false,
        },
        // `xml.parsers.expat` — verbatim CPython over WeavePy's native
        // `pyexpat`. Registering the package + this thin `from pyexpat import *`
        // shim is what lets the verbatim `xml.dom` package below drive the
        // native parser (`xml.parsers.expat` was otherwise unresolved even
        // though `pyexpat` imports fine).
        FrozenSource {
            name: "xml.parsers",
            source: include_str!("python/xml/parsers/__init__.py"),
            is_package: true,
        },
        FrozenSource {
            name: "xml.parsers.expat",
            source: include_str!("python/xml/parsers/expat.py"),
            is_package: false,
        },
        // `xml.sax` — verbatim CPython over the native `pyexpat` (RFC 0056
        // WS3). `expatreader` drives the real expat push parser; `saxutils`
        // provides `escape`/`quoteattr`/`XMLGenerator` used across the
        // ecosystem (docutils, openpyxl's xmlfile, plistlib consumers).
        FrozenSource {
            name: "xml.sax",
            source: include_str!("python/xml/sax/__init__.py"),
            is_package: true,
        },
        FrozenSource {
            name: "xml.sax._exceptions",
            source: include_str!("python/xml/sax/_exceptions.py"),
            is_package: false,
        },
        FrozenSource {
            name: "xml.sax.handler",
            source: include_str!("python/xml/sax/handler.py"),
            is_package: false,
        },
        FrozenSource {
            name: "xml.sax.xmlreader",
            source: include_str!("python/xml/sax/xmlreader.py"),
            is_package: false,
        },
        FrozenSource {
            name: "xml.sax.saxutils",
            source: include_str!("python/xml/sax/saxutils.py"),
            is_package: false,
        },
        FrozenSource {
            name: "xml.sax.expatreader",
            source: include_str!("python/xml/sax/expatreader.py"),
            is_package: false,
        },
        // `xml.dom` + `xml.dom.minidom` (and the builders they need) — verbatim
        // CPython. pandas' `DataFrame.to_xml(pretty_print=True)` does
        // `xml.dom.minidom.parseString(out_xml).toprettyxml(indent="  ")`; the
        // verbatim `minidom` guarantees byte-identical pretty-printed output,
        // and `expatbuilder` runs over the native `pyexpat` (non-namespace mode,
        // qualified names verbatim — matching how the input was serialized).
        FrozenSource {
            name: "xml.dom",
            source: include_str!("python/xml/dom/__init__.py"),
            is_package: true,
        },
        FrozenSource {
            name: "xml.dom.minicompat",
            source: include_str!("python/xml/dom/minicompat.py"),
            is_package: false,
        },
        FrozenSource {
            name: "xml.dom.domreg",
            source: include_str!("python/xml/dom/domreg.py"),
            is_package: false,
        },
        FrozenSource {
            name: "xml.dom.NodeFilter",
            source: include_str!("python/xml/dom/NodeFilter.py"),
            is_package: false,
        },
        FrozenSource {
            name: "xml.dom.xmlbuilder",
            source: include_str!("python/xml/dom/xmlbuilder.py"),
            is_package: false,
        },
        FrozenSource {
            name: "xml.dom.expatbuilder",
            source: include_str!("python/xml/dom/expatbuilder.py"),
            is_package: false,
        },
        FrozenSource {
            name: "xml.dom.minidom",
            source: include_str!("python/xml/dom/minidom.py"),
            is_package: false,
        },
        // `xml.dom.pulldom` — verbatim CPython over `xml.sax` (RFC 0056 WS3).
        FrozenSource {
            name: "xml.dom.pulldom",
            source: include_str!("python/xml/dom/pulldom.py"),
            is_package: false,
        },
        // `xmlrpc` — verbatim CPython (RFC 0056 WS3): client marshalling over
        // `xml.parsers.expat`, server over `socketserver`/`http.server`.
        FrozenSource {
            name: "xmlrpc",
            source: include_str!("python/xmlrpc/__init__.py"),
            is_package: true,
        },
        FrozenSource {
            name: "xmlrpc.client",
            source: include_str!("python/xmlrpc/client.py"),
            is_package: false,
        },
        FrozenSource {
            name: "xmlrpc.server",
            source: include_str!("python/xmlrpc/server.py"),
            is_package: false,
        },
        // RFC 0018 — introspection, test infrastructure, exception groups.
        FrozenSource {
            name: "struct",
            source: include_str!("python/struct.py"),
            is_package: false,
        },
        FrozenSource {
            name: "codecs",
            source: include_str!("python/codecs.py"),
            is_package: false,
        },
        // RFC 0040 WS7 — the JIS X 0213:2004 `euc_jis_2004` CJK codec, ported
        // faithfully (incl. its 25 stateful combining sequences) so the codec's
        // incremental *encoder* is stateful — exercised by
        // `test_io.test_seek_with_encoder_state`. Loaded lazily by
        // `codecs._lookup_uncached` (its 70 KB of packed tables stay cold until
        // the encoding is first used).
        FrozenSource {
            name: "_codec_euc_jis_2004",
            source: include_str!("python/_codec_euc_jis_2004.py"),
            is_package: false,
        },
        // RFC 0050 WS3 — the stateful CJK escape codecs CPython implements in
        // Modules/cjkcodecs (hz, iso2022_jp/_1/_2/_2004/_3/_ext, iso2022_kr,
        // johab, shift_jis_2004, shift_jisx0213). Charsets bridge onto the
        // euc_jp/euc_kr/gb2312 backends and the euc_jis_2004 tables; loaded
        // lazily by `codecs._lookup_uncached`.
        FrozenSource {
            name: "_codec_cjk_ext",
            source: include_str!("python/_codec_cjk_ext.py"),
            is_package: false,
        },
        // RFC 0050 WS3 — the stateless CJK DBCS codecs (euc_kr, cp949,
        // euc_jp, cp932, shift_jis, gb2312, gbk, gb18030, big5, cp950,
        // big5hkscs) with CPython-parity mapping tables probed from CPython
        // 3.13 itself (`tools/gen_cjk_dbcs_tables.py`). Loaded lazily by
        // `codecs._lookup_uncached`.
        FrozenSource {
            name: "_cjk_tables",
            source: include_str!("python/_cjk_tables.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_codec_cjk_dbcs",
            source: include_str!("python/_codec_cjk_dbcs.py"),
            is_package: false,
        },
        // Shared plumbing for the three frozen CJK codec modules above:
        // multibytecodec.c's error-callback protocol, the `errors` getset,
        // and `StreamReader.read(None)` support.
        FrozenSource {
            name: "_cjk_common",
            source: include_str!("python/_cjk_common.py"),
            is_package: false,
        },
        // The `_multibytecodec` module surface (CPython's C base types for
        // the CJK codecs). WeavePy's CJK codecs are frozen Python modules,
        // so only the module-level names matter here.
        FrozenSource {
            name: "_multibytecodec",
            source: include_str!("python/_multibytecodec.py"),
            is_package: false,
        },
        // CPython's `Modules/cjkcodecs/_codecs_*.c` `getcodec` entry points,
        // as handles onto the frozen codecs (for the vendored `encodings.*`).
        FrozenSource {
            name: "_codecs_cn",
            source: include_str!("python/_codecs_cn.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_codecs_hk",
            source: include_str!("python/_codecs_hk.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_codecs_iso2022",
            source: include_str!("python/_codecs_iso2022.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_codecs_jp",
            source: include_str!("python/_codecs_jp.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_codecs_kr",
            source: include_str!("python/_codecs_kr.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_codecs_tw",
            source: include_str!("python/_codecs_tw.py"),
            is_package: false,
        },
        FrozenSource {
            name: "weakref",
            source: include_str!("python/weakref.py"),
            is_package: false,
        },
        FrozenSource {
            name: "datetime",
            source: include_str!("python/datetime.py"),
            is_package: false,
        },
        // RFC 0041 WS-datetime: CPython's verbatim pure-Python datetime
        // implementation, imported by the `datetime` shim above and exercised
        // directly by `test_datetime`'s _Pure pass.
        FrozenSource {
            name: "_pydatetime",
            source: include_str!("python/_pydatetime.py"),
            is_package: false,
        },
        // RFC 0057 WS10: `_datetime` accelerator alias over `_pydatetime` —
        // needed by test_types (datetime_CAPI / types.CapsuleType) and the
        // datetimetester type-cache script.
        FrozenSource {
            name: "_datetime",
            source: include_str!("python/_datetime.py"),
            is_package: false,
        },
        // Target-side half of 3.14's `_interpreters.call()` (see the
        // module docstring for the queue protocol).
        FrozenSource {
            name: "_weave_xidata",
            source: include_str!("python/_weave_xidata.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_weave_xicall",
            source: include_str!("python/_weave_xicall.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_contextvars",
            source: include_str!("python/_contextvars.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_weave_capsule",
            source: include_str!("python/_weave_capsule.py"),
            is_package: false,
        },
        // 3.14's C `_types` module (test_types.test_names imports `types`
        // with and without it).
        FrozenSource {
            name: "_types",
            source: include_str!("python/_types.py"),
            is_package: false,
        },
        FrozenSource {
            name: "linecache",
            source: include_str!("python/linecache.py"),
            is_package: false,
        },
        FrozenSource {
            name: "reprlib",
            source: include_str!("python/reprlib.py"),
            is_package: false,
        },
        FrozenSource {
            name: "warnings",
            source: include_str!("python/warnings.py"),
            is_package: false,
        },
        FrozenSource {
            name: "traceback",
            source: include_str!("python/traceback.py"),
            is_package: false,
        },
        FrozenSource {
            name: "inspect",
            source: include_str!("python/inspect.py"),
            is_package: false,
        },
        FrozenSource {
            name: "contextvars",
            source: include_str!("python/contextvars.py"),
            is_package: false,
        },
        // RFC 0048 — CPython 3.13's *verbatim* `logging` package
        // (`LoggerAdapter`, `logging.config`, `logging.handlers`), replacing
        // the 867-line single-file shim that gated `test_logging` and most
        // real applications' logging setup.
        FrozenSource {
            name: "logging",
            source: include_str!("python/logging/__init__.py"),
            is_package: true,
        },
        FrozenSource {
            name: "logging.config",
            source: include_str!("python/logging/config.py"),
            is_package: false,
        },
        FrozenSource {
            name: "logging.handlers",
            source: include_str!("python/logging/handlers.py"),
            is_package: false,
        },
        // RFC 0048 — CPython 3.13's *verbatim* `unittest` package (the
        // 1,900-line shim it replaces mis-shaped the long tail:
        // `addTypeEqualityFunc`, loader discovery, `TextTestRunner`
        // duration reporting, `IsolatedAsyncioTestCase`, mock autospec).
        FrozenSource {
            name: "unittest",
            source: include_str!("python/unittest/__init__.py"),
            is_package: true,
        },
        FrozenSource {
            name: "unittest.util",
            source: include_str!("python/unittest/util.py"),
            is_package: false,
        },
        FrozenSource {
            name: "unittest.result",
            source: include_str!("python/unittest/result.py"),
            is_package: false,
        },
        FrozenSource {
            name: "unittest.case",
            source: include_str!("python/unittest/case.py"),
            is_package: false,
        },
        FrozenSource {
            name: "unittest.suite",
            source: include_str!("python/unittest/suite.py"),
            is_package: false,
        },
        FrozenSource {
            name: "unittest.loader",
            source: include_str!("python/unittest/loader.py"),
            is_package: false,
        },
        FrozenSource {
            name: "unittest.main",
            source: include_str!("python/unittest/main.py"),
            is_package: false,
        },
        FrozenSource {
            name: "unittest.runner",
            source: include_str!("python/unittest/runner.py"),
            is_package: false,
        },
        FrozenSource {
            name: "unittest.signals",
            source: include_str!("python/unittest/signals.py"),
            is_package: false,
        },
        FrozenSource {
            name: "unittest.async_case",
            source: include_str!("python/unittest/async_case.py"),
            is_package: false,
        },
        FrozenSource {
            name: "unittest._log",
            source: include_str!("python/unittest/_log.py"),
            is_package: false,
        },
        FrozenSource {
            name: "unittest.mock",
            source: include_str!("python/unittest/mock.py"),
            is_package: false,
        },
        FrozenSource {
            name: "unittest.__main__",
            source: include_str!("python/unittest/__main__.py"),
            is_package: false,
        },
        // `doctest` (RFC 0034) — interactive-example testing, used by
        // `test.support.run_doctest` and stdlib self-tests.
        FrozenSource {
            name: "doctest",
            source: include_str!("python/doctest.py"),
            is_package: false,
        },
        // RFC 0048 — verbatim CPython modules the verbatim `unittest` /
        // `test.support` stack imports (plus commonly-imported gaps).
        FrozenSource {
            name: "difflib",
            source: include_str!("python/difflib.py"),
            is_package: false,
        },
        FrozenSource {
            name: "getpass",
            source: include_str!("python/getpass.py"),
            is_package: false,
        },
        FrozenSource {
            name: "fileinput",
            source: include_str!("python/fileinput.py"),
            is_package: false,
        },
        FrozenSource {
            name: "pickletools",
            source: include_str!("python/pickletools.py"),
            is_package: false,
        },
        // `_opcode` — the 3.14 accelerator surface `opcode`, `dis` and
        // `test.support` import (specialization gate, opcode predicates,
        // stack effects). Generated by `tools/gen_opcode_shim.py` from
        // CPython's opcode metadata over the verbatim `_opcode_metadata`
        // tables (RFC 0077 WS9).
        FrozenSource {
            name: "_opcode",
            source: include_str!("python/_opcode.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_opcode_metadata",
            source: include_str!("python/_opcode_metadata.py"),
            is_package: false,
        },
        // `_suggestions` — `Modules/_suggestions.c`'s `_generate_suggestions`
        // (bounded Levenshtein), which `traceback` prefers for "Did you
        // mean" hints and the 3.14 SyntaxError keyword-typo probe.
        FrozenSource {
            name: "_suggestions",
            source: include_str!("python/_suggestions.py"),
            is_package: false,
        },
        // `_remote_debugging` — 3.14's remote stack unwinder. The result
        // struct sequences are real (`asyncio.tools` imports `FrameInfo`
        // at load time, so `python -m asyncio` needs them); attaching to
        // another process isn't available, so `RemoteUnwinder(pid)`
        // raises like the C module does without attach permission.
        FrozenSource {
            name: "_remote_debugging",
            source: include_str!("python/_remote_debugging.py"),
            is_package: false,
        },
        // RFC 0034 — the `test` package: CPython's regression-test
        // harness glue. `test.support` (+ 3.13 helper submodules) is the
        // import-time prerequisite for every `Lib/test/test_*.py`;
        // `test.libregrtest` + `test.__main__` drive `weavepy -m test`.
        FrozenSource {
            name: "test",
            source: include_str!("python/test_init.py"),
            is_package: true,
        },
        // RFC 0048 — CPython 3.13's *verbatim* `test.support` package,
        // replacing the incremental shim. The verbatim `__init__` is what
        // unlocks the ~10 suites that failed at import on missing helpers
        // (`run_code`, `requires_limited_api`, `iter_builtin_types`,
        // `load_package_tests`, `skip_if_buggy_ucrt_strfptime`, …).
        FrozenSource {
            name: "test.support",
            source: include_str!("python/test_support/__init__.py"),
            is_package: true,
        },
        FrozenSource {
            name: "test.support.os_helper",
            source: include_str!("python/test_support/os_helper.py"),
            is_package: false,
        },
        FrozenSource {
            name: "test.support.import_helper",
            source: include_str!("python/test_support/import_helper.py"),
            is_package: false,
        },
        FrozenSource {
            name: "test.support.warnings_helper",
            source: include_str!("python/test_support/warnings_helper.py"),
            is_package: false,
        },
        FrozenSource {
            name: "test.support.threading_helper",
            source: include_str!("python/test_support/threading_helper.py"),
            is_package: false,
        },
        FrozenSource {
            name: "test.support.script_helper",
            source: include_str!("python/test_support/script_helper.py"),
            is_package: false,
        },
        FrozenSource {
            name: "test.support.socket_helper",
            source: include_str!("python/test_support/socket_helper.py"),
            is_package: false,
        },
        FrozenSource {
            name: "test.support.hashlib_helper",
            source: include_str!("python/test_support/hashlib_helper.py"),
            is_package: false,
        },
        FrozenSource {
            name: "test.support.i18n_helper",
            source: include_str!("python/test_support/i18n_helper.py"),
            is_package: false,
        },
        FrozenSource {
            name: "test.support.testcase",
            source: include_str!("python/test_support/testcase.py"),
            is_package: false,
        },
        FrozenSource {
            name: "test.support.numbers",
            source: include_str!("python/test_support/numbers.py"),
            is_package: false,
        },
        FrozenSource {
            name: "test.support.logging_helper",
            source: include_str!("python/test_support/logging_helper.py"),
            is_package: false,
        },
        FrozenSource {
            name: "test.support.ast_helper",
            source: include_str!("python/test_support/ast_helper.py"),
            is_package: false,
        },
        FrozenSource {
            name: "test.support.bytecode_helper",
            source: include_str!("python/test_support/bytecode_helper.py"),
            is_package: false,
        },
        FrozenSource {
            name: "test.support.asynchat",
            source: include_str!("python/test_support/asynchat.py"),
            is_package: false,
        },
        FrozenSource {
            name: "test.support.asyncore",
            source: include_str!("python/test_support/asyncore.py"),
            is_package: false,
        },
        FrozenSource {
            name: "test.support.smtpd",
            source: include_str!("python/test_support/smtpd.py"),
            is_package: false,
        },
        FrozenSource {
            name: "test.support.refleak_helper",
            source: include_str!("python/test_support/refleak_helper.py"),
            is_package: false,
        },
        // `test.tokenizedata`: vendored lexer/tokenizer fixtures.
        // `test_unicode_identifiers` imports `badsyntax_3131` to assert the
        // exact `SyntaxError` for an invalid PEP 3131 identifier (`€`).
        FrozenSource {
            name: "test.tokenizedata",
            source: include_str!("python/test_tokenizedata_init.py"),
            is_package: true,
        },
        FrozenSource {
            name: "test.tokenizedata.badsyntax_3131",
            source: include_str!("python/test_tokenizedata_badsyntax_3131.py"),
            is_package: false,
        },
        // `test.string_tests`: the shared CommonTest/MixinStrUnicodeUserStringTest
        // base classes that `test_bytes`/`test_bytearray`/`test_str` derive
        // from. Carried verbatim from CPython 3.13.
        FrozenSource {
            name: "test.string_tests",
            source: include_str!("python/test_string_tests.py"),
            is_package: false,
        },
        // `test.seq_tests` / `test.list_tests`: shared sequence/list test
        // mixins (verbatim CPython 3.13) that `test_bytes`/`test_list`/
        // `test_tuple`/`test_deque` and friends import.
        FrozenSource {
            name: "test.seq_tests",
            source: include_str!("python/test_seq_tests.py"),
            is_package: false,
        },
        FrozenSource {
            name: "test.list_tests",
            source: include_str!("python/test_list_tests.py"),
            is_package: false,
        },
        // `test.test_grammar` / `test.test_unpack_ex`: verbatim CPython 3.13
        // sources. `test_ast.ASTHelpers_Test.test_stdlib_validates` parses and
        // validates these two files from the installed stdlib tree.
        FrozenSource {
            name: "test.test_grammar",
            source: include_str!("python/test_test_grammar.py"),
            is_package: false,
        },
        FrozenSource {
            name: "test.test_unpack_ex",
            source: include_str!("python/test_test_unpack_ex.py"),
            is_package: false,
        },
        // `test.pickletester` / `test.picklecommon`: verbatim CPython 3.13
        // pickle test matrix (RFC 0057 WS8) — `test_pickle`,
        // `test_pickletools`, and `test_copyreg` all import from it.
        FrozenSource {
            name: "test.pickletester",
            source: include_str!("python/test_pickletester.py"),
            is_package: false,
        },
        FrozenSource {
            name: "test.picklecommon",
            source: include_str!("python/test_picklecommon.py"),
            is_package: false,
        },
        // `test.test_longexp` (verbatim, 10 lines): CPython's own
        // `test_cmd_line.test_relativedir_bug46421` runs
        // `python -m unittest test/test_longexp.py`, which the unittest
        // loader resolves to the *stdlib* `test.test_longexp` module.
        FrozenSource {
            name: "test.test_longexp",
            source: include_str!("python/test_longexp.py"),
            is_package: false,
        },
        // `test.__main__` / `test.regrtest`: drive `weavepy -m test` and
        // `weavepy -m test.regrtest`. The runner itself lives in the
        // `test.libregrtest` package below.
        FrozenSource {
            name: "test.__main__",
            source: include_str!("python/test_main.py"),
            is_package: false,
        },
        FrozenSource {
            name: "test.regrtest",
            source: include_str!("python/test_regrtest.py"),
            is_package: false,
        },
        // RFC 0060 — CPython's *verbatim* `test.libregrtest` package
        // (3.14, from `vendor/cpython314/Lib/test/libregrtest/`; RFC 0077
        // re-vendored it and added `parallel_case`), replacing the
        // RFC 0036 shim whose partial `result.py` shadowed the real one
        // (`test_regrtest` imports `TestStats` at module scope).
        FrozenSource {
            name: "test.libregrtest",
            source: include_str!("python/test_libregrtest/__init__.py"),
            is_package: true,
        },
        FrozenSource {
            name: "test.libregrtest.cmdline",
            source: include_str!("python/test_libregrtest/cmdline.py"),
            is_package: false,
        },
        FrozenSource {
            name: "test.libregrtest.filter",
            source: include_str!("python/test_libregrtest/filter.py"),
            is_package: false,
        },
        FrozenSource {
            name: "test.libregrtest.findtests",
            source: include_str!("python/test_libregrtest/findtests.py"),
            is_package: false,
        },
        FrozenSource {
            name: "test.libregrtest.logger",
            source: include_str!("python/test_libregrtest/logger.py"),
            is_package: false,
        },
        FrozenSource {
            name: "test.libregrtest.main",
            source: include_str!("python/test_libregrtest/main.py"),
            is_package: false,
        },
        FrozenSource {
            name: "test.libregrtest.parallel_case",
            source: include_str!("python/test_libregrtest/parallel_case.py"),
            is_package: false,
        },
        FrozenSource {
            name: "test.libregrtest.pgo",
            source: include_str!("python/test_libregrtest/pgo.py"),
            is_package: false,
        },
        FrozenSource {
            name: "test.libregrtest.refleak",
            source: include_str!("python/test_libregrtest/refleak.py"),
            is_package: false,
        },
        FrozenSource {
            name: "test.libregrtest.result",
            source: include_str!("python/test_libregrtest/result.py"),
            is_package: false,
        },
        FrozenSource {
            name: "test.libregrtest.results",
            source: include_str!("python/test_libregrtest/results.py"),
            is_package: false,
        },
        FrozenSource {
            name: "test.libregrtest.run_workers",
            source: include_str!("python/test_libregrtest/run_workers.py"),
            is_package: false,
        },
        FrozenSource {
            name: "test.libregrtest.runtests",
            source: include_str!("python/test_libregrtest/runtests.py"),
            is_package: false,
        },
        FrozenSource {
            name: "test.libregrtest.save_env",
            source: include_str!("python/test_libregrtest/save_env.py"),
            is_package: false,
        },
        FrozenSource {
            name: "test.libregrtest.setup",
            source: include_str!("python/test_libregrtest/setup.py"),
            is_package: false,
        },
        FrozenSource {
            name: "test.libregrtest.single",
            source: include_str!("python/test_libregrtest/single.py"),
            is_package: false,
        },
        FrozenSource {
            name: "test.libregrtest.testresult",
            source: include_str!("python/test_libregrtest/testresult.py"),
            is_package: false,
        },
        FrozenSource {
            name: "test.libregrtest.tsan",
            source: include_str!("python/test_libregrtest/tsan.py"),
            is_package: false,
        },
        FrozenSource {
            name: "test.libregrtest.utils",
            source: include_str!("python/test_libregrtest/utils.py"),
            is_package: false,
        },
        FrozenSource {
            name: "test.libregrtest.win_utils",
            source: include_str!("python/test_libregrtest/win_utils.py"),
            is_package: false,
        },
        FrozenSource {
            name: "test.libregrtest.worker",
            source: include_str!("python/test_libregrtest/worker.py"),
            is_package: false,
        },
        FrozenSource {
            name: "runpy",
            source: include_str!("python/runpy.py"),
            is_package: false,
        },
        // RFC 0040 WS5 — import modules from ZIP archives on `sys.path`
        // (PEP 273). Self-contained reimplementation over the frozen
        // `zipfile`; plugs into `sys.path_hooks` for the Python `find_spec`
        // path and is reached by the Rust loader's meta-path fallback below.
        FrozenSource {
            name: "zipimport",
            source: include_str!("python/zipimport.py"),
            is_package: false,
        },
        // RFC 0040 WS5 — bridge the Rust import loader to `sys.meta_path`
        // for module kinds it doesn't resolve natively (zip archives,
        // sourceless `.pyc` via a custom finder). Called from `load_one`.
        FrozenSource {
            name: "_weave_import_fallback",
            source: include_str!("python/_weave_import_fallback.py"),
            is_package: false,
        },
        FrozenSource {
            name: "codeop",
            source: include_str!("python/codeop.py"),
            is_package: false,
        },
        FrozenSource {
            name: "code",
            source: include_str!("python/code.py"),
            is_package: false,
        },
        // Compression wrappers (RFC 0019).
        // Shared buffered/decompress reader used by gzip/bz2/lzma (CPython
        // `Lib/_compression.py`, ported verbatim).
        FrozenSource {
            name: "gzip",
            source: include_str!("python/gzip.py"),
            is_package: false,
        },
        FrozenSource {
            name: "bz2",
            source: include_str!("python/bz2.py"),
            is_package: false,
        },
        FrozenSource {
            name: "lzma",
            source: include_str!("python/lzma.py"),
            is_package: false,
        },
        // RFC 0040 WS8 — `zipfile` is CPython 3.13's faithful package
        // (`zipfile/__init__.py` + the `zipfile._path` Path accessor), not
        // the old custom single-module shim. Bundled verbatim and frozen as
        // a package so `zipfile.Path`, `PyZipFile`, ZIP64, per-file
        // compression, `mkdir`, `testzip`, etc. all work.
        // RFC 0068 WS8 — `zipapp` is CPython 3.13's Lib/zipapp.py verbatim
        // (test_pdb imports it to build .pyz targets for `pdb -m`).
        FrozenSource {
            name: "zipapp",
            source: include_str!("python/zipapp.py"),
            is_package: false,
        },
        FrozenSource {
            name: "zipfile",
            source: include_str!("python/zipfile.py"),
            is_package: true,
        },
        FrozenSource {
            name: "zipfile._path",
            source: include_str!("python/zipfile__path.py"),
            is_package: true,
        },
        FrozenSource {
            name: "zipfile._path.glob",
            source: include_str!("python/zipfile__path_glob.py"),
            is_package: false,
        },
        // `python -m zipfile` runs the package's `__main__` (runpy redirects
        // `<pkg>` -> `<pkg>.__main__`); ship it so the CLI works.
        FrozenSource {
            name: "zipfile.__main__",
            source: include_str!("python/zipfile__main__.py"),
            is_package: false,
        },
        FrozenSource {
            name: "tarfile",
            source: include_str!("python/tarfile.py"),
            is_package: false,
        },
        // RFC 0056 WS1: CPython 3.13's *verbatim* `sqlite3` package over
        // the native `_sqlite3` core (`stdlib/sqlite3_native/`).
        FrozenSource {
            name: "sqlite3",
            source: include_str!("python/sqlite3/__init__.py"),
            is_package: true,
        },
        FrozenSource {
            name: "sqlite3.dbapi2",
            source: include_str!("python/sqlite3/dbapi2.py"),
            is_package: false,
        },
        FrozenSource {
            name: "sqlite3.dump",
            source: include_str!("python/sqlite3/dump.py"),
            is_package: false,
        },
        FrozenSource {
            name: "sqlite3.__main__",
            source: include_str!("python/sqlite3/__main__.py"),
            is_package: false,
        },
        FrozenSource {
            name: "copyreg",
            source: include_str!("python/copyreg.py"),
            is_package: false,
        },
        // CPython 3.13 `Lib/colorsys.py`, adopted verbatim (RFC 0056 WS5):
        // rich's color engine imports `rgb_to_hls` at module load.
        FrozenSource {
            name: "colorsys",
            source: include_str!("python/colorsys.py"),
            is_package: false,
        },
        // CPython 3.13 `Lib/netrc.py`, adopted verbatim (RFC 0056 WS5):
        // aiohttp's helpers import it at module load.
        FrozenSource {
            name: "netrc",
            source: include_str!("python/netrc.py"),
            is_package: false,
        },
        // CPython 3.13 `Lib/graphlib.py`, adopted verbatim (RFC 0051):
        // pure Python, no C accelerator, used by `test_genericalias`.
        FrozenSource {
            name: "graphlib",
            source: include_str!("python/graphlib.py"),
            is_package: false,
        },
        // CPython 3.13 `Lib/mailbox.py`, adopted verbatim (RFC 0051):
        // pure Python over the already-frozen `email` package; `fcntl`
        // is an optional import it guards itself.
        FrozenSource {
            name: "mailbox",
            source: include_str!("python/mailbox.py"),
            is_package: false,
        },
        // CPython 3.13 `Lib/filecmp.py`, adopted verbatim (RFC 0051):
        // pure Python over `os`/`stat`/`itertools`.
        FrozenSource {
            name: "filecmp",
            source: include_str!("python/filecmp.py"),
            is_package: false,
        },
        FrozenSource {
            name: "pickle",
            source: include_str!("python/pickle.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_compat_pickle",
            source: include_str!("python/_compat_pickle.py"),
            is_package: false,
        },
        // `_pickle` — pure-Python aliases of pickle's implementation (RFC
        // 0057 WS8): `test_pickle`'s "C" lanes import Pickler/Unpickler/
        // PickleBuffer from it directly.
        FrozenSource {
            name: "_pickle",
            source: include_str!("python/_pickle.py"),
            is_package: false,
        },
        FrozenSource {
            name: "shelve",
            source: include_str!("python/shelve.py"),
            is_package: false,
        },
        // `dbm` — verbatim CPython 3.13 (RFC 0057 WS8). The `__init__` picks
        // a backend lazily; only the pure-Python `dumb` and the
        // `sqlite3`-backed backends are carried (`gnu`/`ndbm` need C libs
        // and `whichdb` degrades gracefully without them).
        FrozenSource {
            name: "dbm",
            source: include_str!("python/dbm/__init__.py"),
            is_package: true,
        },
        FrozenSource {
            name: "dbm.dumb",
            source: include_str!("python/dbm/dumb.py"),
            is_package: false,
        },
        FrozenSource {
            name: "dbm.sqlite3",
            source: include_str!("python/dbm/sqlite3.py"),
            is_package: false,
        },
        FrozenSource {
            name: "fractions",
            source: include_str!("python/fractions.py"),
            is_package: false,
        },
        FrozenSource {
            name: "decimal",
            source: include_str!("python/decimal.py"),
            is_package: false,
        },
        // Full CPython pure-Python decimal (IEEE 754-2008: NaN/Infinity,
        // contexts, traps, exact float/Decimal comparison + hashing). The
        // `decimal` shim above re-exports this via `sys.modules` like CPython.
        FrozenSource {
            name: "_pydecimal",
            source: include_str!("python/_pydecimal.py"),
            is_package: false,
        },
        // RFC 0057 WS7 — `_decimal` accelerator identity: a fork of
        // _pydecimal patched to expose the C-accelerator surface
        // (mpdec constants, immutability, SignalDict, validation) that
        // test_decimal probes. Lib/decimal.py adopts it via
        // `from _decimal import *`.
        FrozenSource {
            name: "_decimal",
            source: include_str!("python/_decimal.py"),
            is_package: false,
        },
        FrozenSource {
            name: "py_compile",
            source: include_str!("python/py_compile.py"),
            is_package: false,
        },
        FrozenSource {
            name: "compileall",
            source: include_str!("python/compileall.py"),
            is_package: false,
        },
        // RFC 0020 — bootstrap modules for the "real `python(1)`" arc.
        FrozenSource {
            name: "site",
            source: include_str!("python/site.py"),
            is_package: false,
        },
        FrozenSource {
            name: "importlib",
            source: include_str!("python/importlib_init.py"),
            is_package: true,
        },
        FrozenSource {
            name: "importlib.machinery",
            source: include_str!("python/importlib_machinery.py"),
            is_package: false,
        },
        FrozenSource {
            name: "importlib.util",
            source: include_str!("python/importlib_util.py"),
            is_package: false,
        },
        FrozenSource {
            name: "importlib._abc",
            source: include_str!("python/importlib__abc.py"),
            is_package: false,
        },
        FrozenSource {
            name: "importlib.abc",
            source: include_str!("python/importlib_abc.py"),
            is_package: false,
        },
        // CPython's `importlib/metadata/` package, frozen verbatim
        // (RFC 0068 WS4 — test_importlib.metadata exercises internals
        // like `_unique` that the old single-file digest lacked).
        FrozenSource {
            name: "importlib.metadata",
            source: include_str!("python/importlib_metadata_pkg/__init__.py"),
            is_package: true,
        },
        FrozenSource {
            name: "importlib.metadata._adapters",
            source: include_str!("python/importlib_metadata_pkg/_adapters.py"),
            is_package: false,
        },
        FrozenSource {
            name: "importlib.metadata._collections",
            source: include_str!("python/importlib_metadata_pkg/_collections.py"),
            is_package: false,
        },
        FrozenSource {
            name: "importlib.metadata._functools",
            source: include_str!("python/importlib_metadata_pkg/_functools.py"),
            is_package: false,
        },
        FrozenSource {
            name: "importlib.metadata._itertools",
            source: include_str!("python/importlib_metadata_pkg/_itertools.py"),
            is_package: false,
        },
        FrozenSource {
            name: "importlib.metadata._meta",
            source: include_str!("python/importlib_metadata_pkg/_meta.py"),
            is_package: false,
        },
        FrozenSource {
            name: "importlib.metadata._text",
            source: include_str!("python/importlib_metadata_pkg/_text.py"),
            is_package: false,
        },
        FrozenSource {
            name: "importlib.metadata.diagnose",
            source: include_str!("python/importlib_metadata_pkg/diagnose.py"),
            is_package: false,
        },
        // CPython's `importlib/resources/` package, frozen verbatim
        // (RFC 0055 WS5). A *package* since 3.11: pytest's assertion
        // rewriter imports `importlib.resources.abc` /
        // `importlib.resources.readers` directly, and `test_zipfile`
        // isinstance-checks `zipfile.Path` against the
        // runtime-checkable `Traversable` Protocol.
        FrozenSource {
            name: "importlib.resources",
            source: include_str!("python/importlib_resources/__init__.py"),
            is_package: true,
        },
        FrozenSource {
            name: "importlib.resources.abc",
            source: include_str!("python/importlib_resources/abc.py"),
            is_package: false,
        },
        FrozenSource {
            name: "importlib.resources.readers",
            source: include_str!("python/importlib_resources/readers.py"),
            is_package: false,
        },
        FrozenSource {
            name: "importlib.resources.simple",
            source: include_str!("python/importlib_resources/simple.py"),
            is_package: false,
        },
        FrozenSource {
            name: "importlib.resources._adapters",
            source: include_str!("python/importlib_resources/_adapters.py"),
            is_package: false,
        },
        FrozenSource {
            name: "importlib.resources._common",
            source: include_str!("python/importlib_resources/_common.py"),
            is_package: false,
        },
        FrozenSource {
            name: "importlib.resources._functional",
            source: include_str!("python/importlib_resources/_functional.py"),
            is_package: false,
        },
        FrozenSource {
            name: "importlib.resources._itertools",
            source: include_str!("python/importlib_resources/_itertools.py"),
            is_package: false,
        },
        // 3.10-era location, kept by CPython as an alias module.
        FrozenSource {
            name: "importlib.readers",
            source: include_str!("python/importlib_readers.py"),
            is_package: false,
        },
        // The `_frozen_importlib*` builtin names CPython freezes; the
        // verbatim `zipimport` (RFC 0055 WS3) imports them directly.
        // (The dotted-name source files are staged on disk via
        // stdlib_tree's DATA_FILES.)
        // `importlib._bootstrap` / `importlib._bootstrap_external` are
        // deliberately *not* registered under their dotted names:
        // `importlib/__init__.py` aliases them in `sys.modules`, and a
        // second frozen execution would mint duplicate classes whose
        // `__module__` says 'importlib._bootstrap' — builtin.test_loader
        // asserts `BuiltinImporter.__module__ == '_frozen_importlib'`.
        FrozenSource {
            name: "_frozen_importlib",
            source: include_str!("python/importlib_bootstrap.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_frozen_importlib_external",
            source: include_str!("python/importlib_bootstrap_external.py"),
            is_package: false,
        },
        FrozenSource {
            name: "pkgutil",
            source: include_str!("python/pkgutil.py"),
            is_package: false,
        },
        // RFC 0037 WS8 — pydoc and its dependency closure.
        FrozenSource {
            name: "pydoc",
            source: include_str!("python/pydoc.py"),
            is_package: false,
        },
        FrozenSource {
            name: "token",
            source: include_str!("python/token.py"),
            is_package: false,
        },
        FrozenSource {
            name: "tokenize",
            source: include_str!("python/tokenize.py"),
            is_package: false,
        },
        // RFC 0052 — `TokenizerIter` shim over the native
        // `_tokenize_core` lexer port (CPython's `_tokenize` C module).
        FrozenSource {
            name: "_tokenize",
            source: include_str!("python/_tokenize.py"),
            is_package: false,
        },
        // RFC 0053 WS4 — verbatim CPython `sysconfig` package over a
        // WeavePy-generated `_sysconfigdata` (CPython generates that
        // module during its autoconf build; ours computes the same
        // variables from the running interpreter). Registered under
        // the platform-derived names `_get_sysconfigdata_name()`
        // produces with `sys.abiflags == ''` and CPython's multiarch
        // tags (RFC 0055 WS1); only the compile target's name resolves
        // at runtime, so registering every platform's spelling is
        // harmless.
        FrozenSource {
            name: "sysconfig",
            source: include_str!("python/sysconfig/__init__.py"),
            is_package: true,
        },
        FrozenSource {
            name: "sysconfig.__main__",
            source: include_str!("python/sysconfig/__main__.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_sysconfigdata__darwin_darwin",
            source: include_str!("python/_weave_sysconfigdata.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_sysconfigdata__linux_x86_64-linux-gnu",
            source: include_str!("python/_weave_sysconfigdata.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_sysconfigdata__linux_aarch64-linux-gnu",
            source: include_str!("python/_weave_sysconfigdata.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_sitebuiltins",
            source: include_str!("python/_sitebuiltins.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_osx_support",
            source: include_str!("python/_osx_support.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_pyrepl",
            source: include_str!("python/_pyrepl_init.py"),
            is_package: true,
        },
        FrozenSource {
            name: "_pyrepl.pager",
            source: include_str!("python/_pyrepl_pager.py"),
            is_package: false,
        },
        // RFC 0055 WS4 — `_pyrepl.console` / `_pyrepl.main` (verbatim):
        // `asyncio.__main__` (the `python -m asyncio` REPL) imports
        // `InteractiveColoredConsole` and `CAN_USE_PYREPL` from them.
        // With a non-tty stdin `CAN_USE_PYREPL` computes False and the
        // console falls back to plain `code.InteractiveConsole.interact`.
        FrozenSource {
            name: "_pyrepl.console",
            source: include_str!("python/_pyrepl_console.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_pyrepl.main",
            source: include_str!("python/_pyrepl_main.py"),
            is_package: false,
        },
        // RFC 0077 WS11 — 3.14's `pdb` imports `_pyrepl.utils` for
        // syntax-colored listings (`gen_colors`); verbatim with its two
        // leaf dependencies.
        FrozenSource {
            name: "_pyrepl.utils",
            source: include_str!("python/_pyrepl_utils.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_pyrepl.types",
            source: include_str!("python/_pyrepl_types.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_pyrepl.trace",
            source: include_str!("python/_pyrepl_trace.py"),
            is_package: false,
        },
        // RFC 0077 — the rest of 3.14.7's `_pyrepl` package, verbatim, so
        // `test_pyrepl` (`from _pyrepl import terminfo`, `unix_console`,
        // `readline`, …) resolves against the staged stdlib. The
        // interactive REPL itself stays Rust-native.
        FrozenSource {
            name: "_pyrepl.__main__",
            source: include_str!("python/_pyrepl___main__.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_pyrepl._module_completer",
            source: include_str!("python/_pyrepl__module_completer.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_pyrepl._threading_handler",
            source: include_str!("python/_pyrepl__threading_handler.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_pyrepl.base_eventqueue",
            source: include_str!("python/_pyrepl_base_eventqueue.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_pyrepl.commands",
            source: include_str!("python/_pyrepl_commands.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_pyrepl.completing_reader",
            source: include_str!("python/_pyrepl_completing_reader.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_pyrepl.fancy_termios",
            source: include_str!("python/_pyrepl_fancy_termios.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_pyrepl.historical_reader",
            source: include_str!("python/_pyrepl_historical_reader.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_pyrepl.input",
            source: include_str!("python/_pyrepl_input.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_pyrepl.keymap",
            source: include_str!("python/_pyrepl_keymap.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_pyrepl.reader",
            source: include_str!("python/_pyrepl_reader.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_pyrepl.readline",
            source: include_str!("python/_pyrepl_readline.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_pyrepl.simple_interact",
            source: include_str!("python/_pyrepl_simple_interact.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_pyrepl.terminfo",
            source: include_str!("python/_pyrepl_terminfo.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_pyrepl.unix_console",
            source: include_str!("python/_pyrepl_unix_console.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_pyrepl.unix_eventqueue",
            source: include_str!("python/_pyrepl_unix_eventqueue.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_pyrepl.windows_console",
            source: include_str!("python/_pyrepl_windows_console.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_pyrepl.windows_eventqueue",
            source: include_str!("python/_pyrepl_windows_eventqueue.py"),
            is_package: false,
        },
        // RFC 0055 WS2 — verbatim CPython `venv` + `ensurepip`
        // packages. The activation scripts and the bundled pip wheel
        // are not `.py` sources; they materialize through the stdlib
        // tree's data-file table (`stdlib_tree::DATA_FILES`), which is
        // where `venv.EnvBuilder.setup_scripts` (`__file__`-relative)
        // and `importlib.resources.files('ensurepip')` find them.
        FrozenSource {
            name: "venv",
            source: include_str!("python/venv/__init__.py"),
            is_package: true,
        },
        FrozenSource {
            name: "venv.__main__",
            source: include_str!("python/venv/__main__.py"),
            is_package: false,
        },
        FrozenSource {
            name: "ensurepip",
            source: include_str!("python/ensurepip/__init__.py"),
            is_package: true,
        },
        FrozenSource {
            name: "ensurepip.__main__",
            source: include_str!("python/ensurepip/__main__.py"),
            is_package: false,
        },
        FrozenSource {
            name: "ensurepip._uninstall",
            source: include_str!("python/ensurepip/_uninstall.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_minipip",
            source: include_str!("python/_minipip.py"),
            is_package: false,
        },
        // Debugger.
        FrozenSource {
            name: "cmd",
            source: include_str!("python/cmd_mod.py"),
            is_package: false,
        },
        FrozenSource {
            name: "bdb",
            source: include_str!("python/bdb_mod.py"),
            is_package: false,
        },
        FrozenSource {
            name: "pdb",
            source: include_str!("python/pdb_mod.py"),
            is_package: false,
        },
        // CPython `Lib/trace.py` verbatim (RFC 0060 WS5): the
        // `trace` CLI / Trace class ride on settrace, which WeavePy
        // implements natively; `test.libregrtest.main` imports it too.
        FrozenSource {
            name: "trace",
            source: include_str!("python/trace_mod.py"),
            is_package: false,
        },
        // RFC 0031 — PEP 684 sub-interpreters friendly frontend.
        FrozenSource {
            name: "interpreters",
            source: include_str!("python/interpreters.py"),
            is_package: false,
        },
        // Small stdlib modules.
        FrozenSource {
            name: "pprint",
            source: include_str!("python/pprint_mod.py"),
            is_package: false,
        },
        // The real CPython `tomllib` package (a vendored `tomli`),
        // verbatim — the earlier trimmed port failed the TOML 1.0
        // conformance suite's error-position and validation edges.
        FrozenSource {
            name: "tomllib",
            source: include_str!("python/tomllib/__init__.py"),
            is_package: true,
        },
        FrozenSource {
            name: "tomllib._parser",
            source: include_str!("python/tomllib/_parser.py"),
            is_package: false,
        },
        FrozenSource {
            name: "tomllib._re",
            source: include_str!("python/tomllib/_re.py"),
            is_package: false,
        },
        FrozenSource {
            name: "tomllib._types",
            source: include_str!("python/tomllib/_types.py"),
            is_package: false,
        },
        FrozenSource {
            name: "configparser",
            source: include_str!("python/configparser_mod.py"),
            is_package: false,
        },
        FrozenSource {
            name: "getopt",
            source: include_str!("python/getopt_mod.py"),
            is_package: false,
        },
        FrozenSource {
            name: "gettext",
            source: include_str!("python/gettext_mod.py"),
            is_package: false,
        },
        FrozenSource {
            name: "optparse",
            source: include_str!("python/optparse_mod.py"),
            is_package: false,
        },
        FrozenSource {
            name: "timeit",
            source: include_str!("python/timeit_mod.py"),
            is_package: false,
        },
        // RFC 0053 WS5 — the full CPython profiling stack: verbatim
        // `profile`/`cProfile`/`pstats` over a `_lsprof` core that
        // aggregates the VM's profile events (incl. the new
        // c_call/c_return/c_exception family).
        FrozenSource {
            name: "profile",
            source: include_str!("python/profile_mod.py"),
            is_package: false,
        },
        FrozenSource {
            name: "cProfile",
            source: include_str!("python/cprofile_mod.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_lsprof",
            source: include_str!("python/_lsprof.py"),
            is_package: false,
        },
        FrozenSource {
            name: "pstats",
            source: include_str!("python/pstats_mod.py"),
            is_package: false,
        },
        // RFC 0057 WS6 — CPython's verbatim `Lib/tracemalloc.py` over the
        // native `_tracemalloc` core.
        FrozenSource {
            name: "tracemalloc",
            source: include_str!("python/tracemalloc_mod.py"),
            is_package: false,
        },
        FrozenSource {
            name: "webbrowser",
            source: include_str!("python/webbrowser_mod.py"),
            is_package: false,
        },
        FrozenSource {
            name: "array",
            source: include_str!("python/array_mod.py"),
            is_package: false,
        },
        FrozenSource {
            name: "plistlib",
            source: include_str!("python/plistlib_mod.py"),
            is_package: false,
        },
        // A real package (CPython `Lib/zoneinfo/`): pandas' Cython
        // `tslibs.timezones` imports `zoneinfo._zoneinfo` directly.
        FrozenSource {
            name: "zoneinfo",
            source: include_str!("python/zoneinfo/__init__.py"),
            is_package: true,
        },
        FrozenSource {
            name: "zoneinfo._zoneinfo",
            source: include_str!("python/zoneinfo/_zoneinfo.py"),
            is_package: false,
        },
        // RFC 0060 — the C-accelerator-shaped `_zoneinfo` (module-state
        // caches + weak-cache validation; `zoneinfo` adopts it at import).
        FrozenSource {
            name: "_zoneinfo",
            source: include_str!("python/_zoneinfo_mod.py"),
            is_package: false,
        },
        // RFC 0023 — fill in the small but commonly-imported stdlib
        // gaps.
        FrozenSource {
            name: "bisect",
            source: include_str!("python/bisect_mod.py"),
            is_package: false,
        },
        FrozenSource {
            name: "operator",
            source: include_str!("python/operator_mod.py"),
            is_package: false,
        },
        FrozenSource {
            name: "copy",
            source: include_str!("python/copy_mod.py"),
            is_package: false,
        },
        FrozenSource {
            name: "stat",
            source: include_str!("python/stat_mod.py"),
            is_package: false,
        },
        FrozenSource {
            name: "genericpath",
            source: include_str!("python/genericpath_mod.py"),
            is_package: false,
        },
        FrozenSource {
            name: "posixpath",
            source: include_str!("python/posixpath_mod.py"),
            is_package: false,
        },
        FrozenSource {
            name: "ntpath",
            source: include_str!("python/ntpath_mod.py"),
            is_package: false,
        },
        // On Windows `urllib.request` does `from nturl2path import …` at
        // module scope, so pip cannot even import without it (RFC 0063).
        FrozenSource {
            name: "nturl2path",
            source: include_str!("python/nturl2path.py"),
            is_package: false,
        },
        FrozenSource {
            name: "textwrap",
            source: include_str!("python/textwrap_mod.py"),
            is_package: false,
        },
        FrozenSource {
            name: "numbers",
            source: include_str!("python/numbers_mod.py"),
            is_package: false,
        },
        FrozenSource {
            name: "statistics",
            source: include_str!("python/statistics_mod.py"),
            is_package: false,
        },
        // RFC 0026 — fill in the last commonly-imported gaps.
        FrozenSource {
            name: "types",
            source: include_str!("python/types_mod.py"),
            is_package: false,
        },
        FrozenSource {
            name: "posix",
            source: include_str!("python/posix_mod.py"),
            is_package: false,
        },
        // Verbatim CPython `os.py`. The *startup* import of `os` stays
        // on the Rust-native fast path (the builtin factory — CPython's
        // frozen-`os` analogue); this registration serves later *fresh*
        // imports, which CPython resolves from `Lib/os.py` source
        // (import_fresh_module('os') → SourceFileLoader spec with a
        // real `cached` pyc; test_pydoc test_synopsis_sourceless).
        FrozenSource {
            name: "os",
            source: include_str!("python/os_source.py"),
            is_package: false,
        },
        FrozenSource {
            name: "nt",
            source: include_str!("python/nt_mod.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_oswalk",
            source: include_str!("python/_oswalk.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_multiprocessing_helpers",
            source: include_str!("python/_multiprocessing_helpers.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_concurrent_process",
            source: include_str!("python/_concurrent_process.py"),
            is_package: false,
        },
        // RFC 0030 — real PyPI client (packaging utils, PEP 517 builds),
        // numpy facade, pytest+pluggy.
        FrozenSource {
            name: "_packaging",
            source: include_str!("python/_packaging.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_pip_resolver",
            source: include_str!("python/_pip_resolver.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_pep517",
            source: include_str!("python/_pep517.py"),
            is_package: false,
        },
        // Expose the WeavePy pip under the canonical `pip` name — via
        // the RFC 0055 WS2 dispatcher, which prefers an installed
        // site-packages pip, honours venv --without-pip semantics
        // (`import pip` fails there), and falls back to the embedded
        // `_minipip` in the base environment.
        FrozenSource {
            name: "pip",
            source: include_str!("python/pip_shim.py"),
            is_package: false,
        },
        // `packaging` is a third-party project on PyPI but extremely
        // commonly imported. Map it to our in-tree `_packaging`.
        FrozenSource {
            name: "packaging",
            source: include_str!("python/packaging_init.py"),
            is_package: true,
        },
        FrozenSource {
            name: "packaging.version",
            source: include_str!("python/packaging_version.py"),
            is_package: false,
        },
        FrozenSource {
            name: "packaging.specifiers",
            source: include_str!("python/packaging_specifiers.py"),
            is_package: false,
        },
        FrozenSource {
            name: "packaging.requirements",
            source: include_str!("python/packaging_requirements.py"),
            is_package: false,
        },
        FrozenSource {
            name: "packaging.markers",
            source: include_str!("python/packaging_markers.py"),
            is_package: false,
        },
        FrozenSource {
            name: "packaging.utils",
            source: include_str!("python/packaging_utils.py"),
            is_package: false,
        },
        FrozenSource {
            name: "packaging.tags",
            source: include_str!("python/packaging_tags.py"),
            is_package: false,
        },
        // numpy-compatible facade over the bundled `_numpylike` C
        // extension. Real numpy code that doesn't reach into the
        // C-level internals "just works".
        FrozenSource {
            name: "_numpy_pure",
            source: include_str!("python/_numpy_pure.py"),
            is_package: false,
        },
        // RFC 0066 WS4: the bundled greenlet distribution — a thin
        // facade over the native `_greenlet` module. A pip-installed
        // cp313 greenlet wheel can never work under WeavePy (it
        // hand-switches CPython's C stack), so the bundled package is
        // the one true greenlet; the matching dist-info ships in the
        // stdlib tree so importlib.metadata sees it installed.
        FrozenSource {
            name: "greenlet",
            source: include_str!("python/greenlet_init.py"),
            is_package: true,
        },
        FrozenSource {
            name: "numpy",
            source: include_str!("python/numpy_init.py"),
            is_package: false,
        },
        // pytest + pluggy shims.
        FrozenSource {
            name: "pluggy",
            source: include_str!("python/_pluggy.py"),
            is_package: false,
        },
        FrozenSource {
            name: "pytest",
            source: include_str!("python/_pytest.py"),
            is_package: false,
        },
        FrozenSource {
            name: "iniconfig",
            source: include_str!("python/iniconfig_mod.py"),
            is_package: false,
        },
        FrozenSource {
            name: "exceptiongroup",
            source: include_str!("python/exceptiongroup_mod.py"),
            is_package: false,
        },
        // RFC 0033 — bytecode & introspection compatibility layer.
        FrozenSource {
            name: "opcode",
            source: include_str!("python/opcode.py"),
            is_package: false,
        },
        FrozenSource {
            name: "dis",
            source: include_str!("python/dis.py"),
            is_package: false,
        },
        FrozenSource {
            name: "ast",
            source: include_str!("python/ast.py"),
            is_package: false,
        },
        FrozenSource {
            name: "symtable",
            source: include_str!("python/symtable.py"),
            is_package: false,
        },
        // RFC 0035 — the `re` package: a faithful port of CPython's
        // secret-labs engine. `_constants` / `_parser` / `_compiler` /
        // `_casefix` are verbatim from CPython 3.13; `_engine` builds the
        // Pattern / Match objects on top of the native `_sre` core.
        FrozenSource {
            name: "re",
            source: include_str!("python/re_init.py"),
            is_package: true,
        },
        FrozenSource {
            name: "re._constants",
            source: include_str!("python/re_constants.py"),
            is_package: false,
        },
        FrozenSource {
            name: "re._casefix",
            source: include_str!("python/re_casefix.py"),
            is_package: false,
        },
        FrozenSource {
            name: "re._parser",
            source: include_str!("python/re_parser.py"),
            is_package: false,
        },
        FrozenSource {
            name: "re._compiler",
            source: include_str!("python/re_compiler.py"),
            is_package: false,
        },
        FrozenSource {
            name: "re._engine",
            source: include_str!("python/re_engine.py"),
            is_package: false,
        },
        // Deprecated 3.x aliases kept for compatibility with code that
        // still imports the pre-3.11 module names.
        FrozenSource {
            name: "sre_constants",
            source: include_str!("python/sre_constants.py"),
            is_package: false,
        },
        FrozenSource {
            name: "sre_parse",
            source: include_str!("python/sre_parse.py"),
            is_package: false,
        },
        FrozenSource {
            name: "sre_compile",
            source: include_str!("python/sre_compile.py"),
            is_package: false,
        },
        // Pure-Python stand-in for CPython's `_testlimitedcapi` C test
        // helper. The conformance suite (e.g. `test_bytes`) imports it at
        // class-body scope; without it the whole module aborts. We supply
        // faithful Python equivalents of the abstract `PySequence_*`
        // wrappers it exercises.
        FrozenSource {
            name: "_testlimitedcapi",
            source: include_str!("python/_testlimitedcapi.py"),
            is_package: false,
        },
        // RFC 0068 WS3 — the PyArg_ParseTuple/ParseTupleAndKeywords
        // emulation (a Python port of `Python/getargs.c`), backing the
        // `getargs_*` fixture family in `_testcapi` (test_getargs).
        FrozenSource {
            name: "_weave_getargs",
            source: include_str!("python/_weave_getargs.py"),
            is_package: false,
        },
        // RFC 0068 WS3 — per-family C-API fixture shims backing the
        // `test_capi` per-leg suites. Each defines `__all__`; the
        // `_testcapi` and `_testlimitedcapi` frozen shims star-import
        // them so tests find fixtures under either module name.
        FrozenSource {
            name: "_weave_capi_bin",
            source: include_str!("python/_weave_capi_bin.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_weave_capi_cont",
            source: include_str!("python/_weave_capi_cont.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_weave_capi_num",
            source: include_str!("python/_weave_capi_num.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_weave_capi_text",
            source: include_str!("python/_weave_capi_text.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_weave_capi_misc",
            source: include_str!("python/_weave_capi_misc.py"),
            is_package: false,
        },
        // Pure-Python stand-in for `_testcapi`, covering the traceback
        // hooks (`exception_print` -> PyErr_Display via the traceback
        // module, `traceback_print` -> PyTraceBack_Print).
        FrozenSource {
            name: "_testcapi",
            source: include_str!("python/_testcapi.py"),
            is_package: false,
        },
        // `_testmultiphase` is intentionally *not* frozen: RFC 0068 WS4
        // compiles CPython's real `Modules/_testmultiphase.c` into the
        // conformance fixture dir (staged onto `sys.path` by the regrtest
        // bootstrap), and a frozen shim here would shadow it in
        // `importlib.util.find_spec` (FrozenImporter outranks PathFinder).
        // The pure-Python stand-in in `python/_testmultiphase.py` still
        // backs `_imp._load_dynamic` when no real `.so` exists.
        // RFC 0068 WS3 — CPython 3.13's low-level `_interpreters` module
        // (PEP 734), a frozen shim over the `_xxsubinterpreters` native
        // core. test_capi.test_misc's SubinterpreterTest references
        // `_interpreters.InterpreterError` directly.
        FrozenSource {
            name: "_interpreters",
            source: include_str!("python/_interpreters.py"),
            is_package: false,
        },
        // RFC 0068 WS5 — CPython 3.13's `_interpchannels` (cross-
        // interpreter channels), adapted onto the same native core.
        // `test.support.interpreters.channels` imports it at module
        // scope (test_types SubinterpreterTests.setUpClass).
        FrozenSource {
            name: "_interpchannels",
            source: include_str!("python/_interpchannels.py"),
            is_package: false,
        },
        // RFC 0068 WS9 — CPython 3.13's `_interpqueues` (cross-
        // interpreter queues), adapted onto the same native core.
        // `test.support.interpreters.queues` imports it at module scope
        // (test___all__ star-imports test.support.interpreters).
        FrozenSource {
            name: "_interpqueues",
            source: include_str!("python/_interpqueues.py"),
            is_package: false,
        },
        // RFC 0057 WS3 — CPython's frozen *test* modules (Python/frozen.c's
        // TEST section, sources verbatim from `Lib/__hello__.py` and
        // `Lib/__phello__/`). `test_frozen` and `test_importlib` import them
        // to probe FrozenImporter semantics: `import __hello__` prints
        // "Hello world!", `__phello__` is a package with a frozen submodule,
        // and `__phello__.ham(.eggs)` is an empty frozen package. Unlike the
        // rest of the frozen stdlib these honour
        // `_imp._override_frozen_modules_for_tests` (see
        // `ModuleCache::frozen_source`) and keep `<frozen …>` identity
        // (FrozenImporter loader, origin='frozen' — see
        // `stdlib_tree::module_path`).
        FrozenSource {
            name: "__hello__",
            source: include_str!("python/__hello__.py"),
            is_package: false,
        },
        FrozenSource {
            name: "__phello__",
            source: include_str!("python/__phello__/__init__.py"),
            is_package: true,
        },
        FrozenSource {
            name: "__phello__.spam",
            source: include_str!("python/__phello__/spam.py"),
            is_package: false,
        },
        FrozenSource {
            name: "__phello__.ham",
            source: include_str!("python/__phello__/ham/__init__.py"),
            is_package: true,
        },
        FrozenSource {
            name: "__phello__.ham.eggs",
            source: include_str!("python/__phello__/ham/eggs.py"),
            is_package: false,
        },
        // Alias rows of CPython's frozen TEST table (`Python/frozen.c`):
        // frozen names whose *code* comes from another module's source.
        // `test_importlib.frozen` asserts FrozenImporter.find_spec resolves
        // them (spec.loader_state.origname carries the alias mapping — see
        // `importlib_machinery.FrozenImporter._ORIGNAME_ALIASES`).
        FrozenSource {
            name: "__hello_alias__",
            source: include_str!("python/__hello__.py"),
            is_package: false,
        },
        FrozenSource {
            name: "__phello_alias__",
            source: include_str!("python/__hello__.py"),
            is_package: true,
        },
        FrozenSource {
            name: "__phello_alias__.spam",
            source: include_str!("python/__hello__.py"),
            is_package: false,
        },
        // In CPython `__hello_only__` freezes `Tools/freeze/flag.py` (a
        // data-only row: no origname, no filename). The source text is
        // irrelevant to the tests; only its *presence* in the table is.
        FrozenSource {
            name: "__hello_only__",
            source: "initialized = True\n",
            is_package: false,
        },
        // RFC 0077 WS8: modules new in (or newly bundled from) CPython 3.14.
        FrozenSource {
            name: "_ast_unparse",
            source: include_str!("python/_ast_unparse.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_py_warnings",
            source: include_str!("python/_py_warnings.py"),
            is_package: false,
        },
        FrozenSource {
            name: "annotationlib",
            source: include_str!("python/annotationlib.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.graph",
            source: include_str!("python/asyncio/graph.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.tools",
            source: include_str!("python/asyncio/tools.py"),
            is_package: false,
        },
        FrozenSource {
            name: "concurrent.futures.interpreter",
            source: include_str!("python/concurrent_futures_interpreter.py"),
            is_package: false,
        },
        FrozenSource {
            name: "concurrent.interpreters",
            source: include_str!("python/concurrent/interpreters/__init__.py"),
            is_package: true,
        },
        FrozenSource {
            name: "concurrent.interpreters._crossinterp",
            source: include_str!("python/concurrent/interpreters/_crossinterp.py"),
            is_package: false,
        },
        FrozenSource {
            name: "concurrent.interpreters._queues",
            source: include_str!("python/concurrent/interpreters/_queues.py"),
            is_package: false,
        },
        FrozenSource {
            name: "pathlib._os",
            source: include_str!("python/pathlib__os.py"),
            is_package: false,
        },
        FrozenSource {
            name: "pathlib.types",
            source: include_str!("python/pathlib_types.py"),
            is_package: false,
        },
        FrozenSource {
            name: "sched",
            source: include_str!("python/sched.py"),
            is_package: false,
        },
        FrozenSource {
            name: "tabnanny",
            source: include_str!("python/tabnanny.py"),
            is_package: false,
        },
        FrozenSource {
            name: "wave",
            source: include_str!("python/wave.py"),
            is_package: false,
        },
        FrozenSource {
            name: "pyclbr",
            source: include_str!("python/pyclbr.py"),
            is_package: false,
        },
        FrozenSource {
            name: "modulefinder",
            source: include_str!("python/modulefinder.py"),
            is_package: false,
        },
        FrozenSource {
            name: "json.__main__",
            source: include_str!("python/json/__main__.py"),
            is_package: false,
        },
        FrozenSource {
            name: "importlib.simple",
            source: include_str!("python/importlib_simple.py"),
            is_package: false,
        },
        FrozenSource {
            name: "this",
            source: include_str!("python/this.py"),
            is_package: false,
        },
        FrozenSource {
            name: "antigravity",
            source: include_str!("python/antigravity.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_apple_support",
            source: include_str!("python/_apple_support.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_android_support",
            source: include_str!("python/_android_support.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_ios_support",
            source: include_str!("python/_ios_support.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_aix_support",
            source: include_str!("python/_aix_support.py"),
            is_package: false,
        },
        FrozenSource {
            name: "xml.etree.cElementTree",
            source: include_str!("python/xml/etree/cElementTree.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_pylong",
            source: include_str!("python/_pylong.py"),
            is_package: false,
        },
        FrozenSource {
            name: "pydoc_data",
            source: include_str!("python/pydoc_data/__init__.py"),
            is_package: true,
        },
        FrozenSource {
            name: "pydoc_data.module_docs",
            source: include_str!("python/pydoc_data/module_docs.py"),
            is_package: false,
        },
        FrozenSource {
            name: "pydoc_data.topics",
            source: include_str!("python/pydoc_data/topics.py"),
            is_package: false,
        },
        // Explicit `<pkg>.__init__` rows (importable spellings of the
        // package init, origname `<<pkg>` in the frozen table).
        FrozenSource {
            name: "__phello__.__init__",
            source: include_str!("python/__phello__/__init__.py"),
            is_package: false,
        },
        FrozenSource {
            name: "__phello__.ham.__init__",
            source: include_str!("python/__phello__/ham/__init__.py"),
            is_package: false,
        },
    ];
    SOURCES
}
