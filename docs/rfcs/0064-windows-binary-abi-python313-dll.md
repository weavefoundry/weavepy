# RFC 0064: The python313.dll wave — Windows binary extensions, the runtime cdylib, MSVC builds, and console Unicode

- **Status**: Accepted
- **Authors**: WeavePy authors
- **Created**: 2026-08-12
- **Tracking issue**: TBD
- **Builds on**: RFC 0063 (the NT-native runtime this wave gives a
  linkable ABI: CRT fds, `_winapi`/`msvcrt`/`winreg`/`_overlapped`,
  the zip artifact, the advisory Windows lanes and the `measured_os`
  stamp), RFC 0062 (the installable header tree, compiler-truthful
  sysconfig, and the dist check matrix whose cext leg this wave
  un-skips on NT), RFC 0043–0047 (the binary ABI: layout-faithful
  mirrors, `PyType_FromSpec`, inline storage, real numpy/Cython —
  everything a `.pyd` calls once its imports resolve), RFC 0022
  (the C-API foundation and its force-link discipline).

## Summary

RFC 0063 made WeavePy a real pure-Python interpreter on Windows and
drew one hard boundary: no C extensions, because a `.pyd` built for
CPython carries a PE import table naming `python313.dll`, and WeavePy
was a static executable with nothing for the loader to bind against.
This wave erases that boundary. The runtime moves into a
**`python313` cdylib** — a new `weavepy-pylib` crate whose Windows
artifact is a real `python313.dll` exporting the full ~682-symbol
C-API surface (the same `#[no_mangle]` set the RFC 0022 force-link
table already enumerates) plus a `weavepy_main` entry point — and
`weavepy.exe` becomes a **thin shim** that locates the DLL (its own
directory, then the `pyvenv.cfg` `home=` chain for venv copies),
loads it, and calls `weavepy_main`. POSIX keeps today's fully-static
binary and `--export-dynamic`/`dynamic_lookup` story, unchanged.
On top of the DLL land the consumers: `ExtensionFileLoader` gains
CPython's `LoadLibraryExW` search semantics and its
"`DLL load failed while importing X`" `ImportError` shape,
`os.add_dll_directory` arrives with the `_AddedDllDirectory`
context manager, the artifact ships `libs\python313.lib` (the
MSVC import library rustc already produces) plus a pyconfig.h that
autolinks it — so `pip install` of both **binary wheels** (numpy,
pandas: the PE import now resolves) and **C sdists** (setuptools →
MSVC → link against `libs\`) works mechanically — and the dist
`check` cext leg un-skips on Windows, compiling and importing a
`.pyd` end-to-end when MSVC is present. `_WindowsConsoleIO`
completes RFC 0063's deferred console-Unicode story with
`ReadConsoleW`/`WriteConsoleW`-backed interactive stdio. The lanes
stay advisory-until-measured exactly as RFC 0063 left them; the
flip-to-blocking baseline transplant remains the named follow-up,
now with the cext legs included in what it measures.

## Motivation

1. **The drop-in claim on Windows currently excludes the packages
   people drop in for.** The RFC 0055/0056/0060 ecosystem story —
   numpy, pandas, cryptography, orjson, charset_normalizer's mypyc
   `.so` — is what "daily driver" means, and none of it can load on
   Windows: `import numpy` finds the binary wheel's
   `_multiarray_umath.cp313-win_amd64.pyd`, the loader maps it, and
   the PE import of `python313.dll` fails before `PyInit_*` is ever
   reachable. Every wave that grows the POSIX ecosystem matrix
   widens the gap on the platform with the largest desktop install
   base.

2. **The boundary was drawn deliberately, and its other side was
   prepared.** RFC 0063's Non-goals named this exact wave: "the
   honest fix (restructuring the workspace so a `python313.dll`
   cdylib exports the C-API and the exe links it) is its own wave."
   The prerequisites are all landed: the header tree installs
   (RFC 0062 WS2), `EXT_SUFFIX` is already truthful
   (`.cp313-win_amd64.pyd`), the NT runtime beneath the C-API is
   proven by the RFC 0063 test battery, and the `measured_os`
   advisory machinery is sitting there waiting for the cext story
   to become measurable.

3. **The export mechanism already exists — it is just aimed at the
   wrong binary format.** The C-API is ~682 `#[no_mangle]` symbols
   kept alive by the `#[used]` `FORCE_LINK` table (RFC 0022) and
   made dlopen-visible by `--export-dynamic` on Linux and Mach-O
   default-export on macOS. PE is the one format where an
   executable's symbols are invisible to the loader by default —
   the same symbol set compiled into a cdylib is exported with no
   new per-symbol work, because rustc builds a cdylib's export
   list from exactly the reachable `#[no_mangle]` surface.

4. **A static-exe Windows Python mis-signals toolchains.** setuptools
   on Windows unconditionally links extensions against
   `{base_exec_prefix}\libs\python313.lib`; the directory not
   existing fails builds with a linker error users cannot act on.
   `sys.dllhandle == 0` tells `ctypes.pythonapi` consumers there is
   no Python DLL. Truthful signals require the DLL to exist.

## CPython reference

- **The DLL split**: on Windows, `python.exe` is a ~30 KB shim
  (`Programs/python.c`) whose `wmain` calls `Py_Main` in
  `python313.dll`; every C-API symbol lives in the DLL
  (`PC/pyconfig.h` defines `MS_COREDLL`/`Py_ENABLE_SHARED`).
  Extensions import `python313.dll` by name; the loader resolves it
  to the already-loaded module in-process.
- **Import-library autolink**: `PC/pyconfig.h` emits
  `#pragma comment(lib,"python313.lib")` when building non-core
  code, so an extension's link step pulls the import library off
  the `/LIBPATH` that distutils/setuptools point at
  `{sys.base_exec_prefix}\libs` (`Lib/distutils/command/build_ext.py`,
  preserved by setuptools' `_distutils`).
- **Extension loading**: `Python/dynload_win.c` calls
  `LoadLibraryExW(path, NULL, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS |
  LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR)` — dependent DLLs resolve from
  the `.pyd`'s own directory, `AddDllDirectory` cookies, System32,
  and application dir; **not** `PATH`, **not** CWD (CPython 3.8+
  behavior, bpo-36085). Failure raises
  `ImportError("DLL load failed while importing {name}: {strerror}")`
  with `name` set.
- **`os.add_dll_directory`**: `Lib/os.py` defines
  `_AddedDllDirectory` (with `close()`, `__enter__`/`__exit__`,
  `repr` as `<AddedDllDirectory({path!r})>`) over
  `nt._add_dll_directory`/`nt._remove_dll_directory`
  (`AddDllDirectory`/`RemoveDllDirectory` in
  `Modules/posixmodule.c`); raises the `os.add_dll_directory`
  audit event.
- **`sys.dllhandle`**: the `HMODULE` of `python313.dll`
  (`PC/getpathp.c` era; today `PC/python_ver_rc.h` sibling code in
  `Python/sysmodule.c` gated on `MS_COREDLL`). `ctypes.pythonapi`
  is `PyDLL(None)` on POSIX but `PyDLL("python dll", handle=
  sys.dllhandle)` on Windows (`Lib/ctypes/__init__.py`).
- **Venv resolution**: CPython venvs use `venvlauncher.exe`;
  python-build-standalone instead copies the base exe and relies on
  `pyvenv.cfg` `home=` pointing at the base prefix — the model
  WeavePy adopted in RFC 0063 WS6 and this wave's shim honours.
- **`_WindowsConsoleIO`**: `Modules/_io/winconsoleio.c` — raw io
  over console handles; `read` via `ReadConsoleW` then
  `WideCharToMultiByte(CP_UTF8)`, `write` via
  `MultiByteToWideChar` then `WriteConsoleW` (chunked; CPython
  caps at 32766 wchars per call), Ctrl-C surfacing as
  `ERROR_OPERATION_ABORTED` → `KeyboardInterrupt` via the signal
  machinery, Ctrl-Z (`\x1a`) as EOF at the start of a read,
  `fileno()` returning the CRT fd, `isatty()` always true.
  `Lib/io.py`/`_pyio.py` route `open()` of console paths
  (`CONIN$`/`CONOUT$`/`CON`) and interactive std streams through it
  when `sys.platform == 'win32'`.

## Detailed design

Five workstreams. WS1 is the restructure everything else consumes;
WS2–WS4 are the consumers (import system, build system, console);
WS5 is verification. The verification channel matches RFC 0063:
`cargo check --target x86_64-pc-windows-msvc` must stay clean
locally for every touched crate (compilation without linking), the
blocking `windows-latest` `cargo test` job grows integration tests
that exercise the DLL for real, and the macOS/Linux gates
(regrtest `--check`, ecosystem `--check`, bench, `weavepy-dist
check`) must hold unchanged.

### WS1 — the runtime cdylib and the thin exe

**The crate split.** `weavepy-cli` today is a single `main.rs`
(~1.7K lines) plus `repl.rs`/`regrtest_cmd.rs`, bin-only. It gains a
**lib target**: the driver logic moves verbatim into
`weavepy-cli/src/lib.rs` behind one public entry point,

```rust
/// Run the WeavePy CLI against this process's real argv/env.
/// Returns the process exit code.
pub fn cli_main() -> i32
```

and the bin `main.rs` shrinks to a platform switch. A new crate
**`crates/weavepy-pylib`** owns the shared library:

```toml
[lib]
name = "python313"
crate-type = ["cdylib"]

[dependencies]
weavepy-cli = { workspace = true }
```

Its `lib.rs` exports the embedding entry points:

- `#[no_mangle] pub extern "C" fn weavepy_main() -> c_int` — calls
  `weavepy_cli::cli_main()`. Argv/env come from the process (on
  Windows, `std::env::args_os` reads `GetCommandLineW`, which is
  process-global and DLL-safe).
- `#[no_mangle] pub unsafe extern "C" fn Py_Main(argc, argv:
  *mut *mut wchar_t) -> c_int` and `Py_BytesMain(argc, argv:
  *mut *mut c_char) -> c_int` — the CPython embedding twins,
  decoding their argv (UTF-16 on Windows, UTF-32 elsewhere; WTF-8
  tolerant like the RFC 0060 `sys.orig_argv` bridge) and calling a
  `cli_main_with_args(Vec<OsString>)` variant. Stock
  `pylifecycle.h` already declares both.

The ~682 C-API symbols need no enumeration: they are `#[no_mangle]
pub extern "C"` items in `weavepy-capi`, which `weavepy-pylib`
links transitively (cli → weavepy umbrella → capi), and rustc
derives a cdylib's PE export table from the reachable `#[no_mangle]`
surface of the whole crate graph. The `#[used]` `FORCE_LINK` table
(RFC 0022) guarantees reachability, exactly as it does for the
static exe today. `weavepy-pylib` calls `weavepy::install_capi_loader()`
inside `weavepy_main` before delegating, same as the CLI does today
(the call is already inside `run_source_with_options_impl`, so this
is belt-and-braces, not new behavior).

**The thin shim.** `weavepy-cli`'s dependency table splits by
target: on `cfg(not(windows))` the bin keeps the full static link
(`fn main` calls `weavepy_cli::cli_main()` from the lib — POSIX
behavior, size, and the `--export-dynamic` build.rs contract are
all byte-identical to today). On `cfg(windows)` the bin does **not**
reference the lib; `main` is a loader:

1. `GetModuleFileNameW` → the exe's own directory; try
   `{exe_dir}\python313.dll` via `LoadLibraryExW(abs_path, NULL,
   LOAD_WITH_ALTERED_SEARCH_PATH)`.
2. If absent (the venv case — RFC 0063 venvs copy the base exe as
   `Scripts\python.exe` and do *not* copy the DLL): read
   `{exe_dir}\..\pyvenv.cfg`, take the `home =` value, and load
   `{home}\python313.dll`.
3. Failing both, a plain `LoadLibraryW(L"python313.dll")` (default
   search order) as the last resort — this is what makes
   `cargo run -p weavepy-cli` work from a target dir where cargo
   placed the DLL, and what a user who split the files across
   `PATH` gets.
4. On failure: a clear two-line error naming the paths probed and
   the wave's contract ("weavepy.exe requires python313.dll from
   the same distribution"), exit code 103 (well clear of Python's
   1/2/120 conventions).
5. `GetProcAddress(dll, "weavepy_main")` → call → `exit(code)`.

Because the Windows bin never touches the runtime crates, the MSVC
linker's archive semantics leave the shim at shim size; the
`[target.'cfg(not(windows))'.dependencies]` split in `Cargo.toml`
makes the independence structural rather than an artifact of
dead-code elimination. `windows-sys` (already a workspace dep) is
the shim's only Windows dependency.

**Process-global state and the DLL boundary.** All interpreter
state lives in the DLL's image (thread-locals, the GIL, the GC
registries); the shim owns nothing but the loader call, so there is
exactly one runtime in the process and a `.pyd`'s
`python313.dll` import binds to the already-loaded module by name —
the same in-process resolution CPython relies on. The RFC 0063
`SetConsoleCtrlHandler` registration happens inside
`weavepy_main`'s init path (it already does — `install_startup_dispositions`
is called from the driver, which now lives in the DLL), so signal
delivery is unchanged.

**`sys.dllhandle`.** The RFC 0063 hardcoded `0` becomes truthful:
`sys::build` on Windows calls
`GetModuleHandleW(w!("python313.dll"))` and publishes the `HMODULE`
as an int — nonzero through the shim (and through any embedder that
loaded the DLL), 0 in statically-linked Rust test harnesses, which
is the honest answer for a process with no Python DLL.
`ctypes.pythonapi` then constructs against the real handle.

**POSIX.** Nothing ships differently: the cdylib crate builds a
`libpython313.so`/`.dylib` as a workspace member (useful for future
embedding work and for keeping the crate honest under
`cargo test --workspace`), but the artifact layout, the static
`bin/weavepy`, and the export mechanics are untouched. Packaging a
POSIX shared library is explicitly out of scope (Non-goals).

### WS2 — `.pyd` loading: search semantics, error shape, `os.add_dll_directory`

**Loader flags.** `weavepy-capi/src/loader.rs` currently opens every
extension via `libloading::Library::new`. That is right on POSIX and
wrong on Windows (it inherits the legacy default search order,
including CWD and `PATH`). The Windows arm switches to
`libloading::os::windows::Library::load_with_flags(path,
LOAD_LIBRARY_SEARCH_DEFAULT_DIRS | LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR)`
— CPython's exact `dynload_win.c` flags, so a wheel's `.pyd` can
resolve its vendored dependent DLLs from its own directory and from
`AddDllDirectory` cookies, and *cannot* pick DLLs off `PATH`/CWD
(the bpo-36085 hardening; delocate/`.libs` layouts depend on the
former, security posture on the latter).

**Error shape.** A failed load on Windows raises
`ImportError("DLL load failed while importing {leaf}: {strerror}")`
with `name`/`path` set — the message tooling and Stack Overflow
muscle memory both match on. `strerror` comes through the RFC 0063
`FormatMessageW` path (trailing CRLF trimmed). POSIX keeps its
dlerror-based message.

**`os.add_dll_directory`.** Rust-native in `os.rs`'s existing
`#[cfg(windows)]` block (WeavePy's `os` is Rust-owned; the frozen
`nt` shim re-exports):

- `os.add_dll_directory(path)` — validates the path is absolute and
  a directory (CPython delegates both to the API's
  `ERROR_INVALID_PARAMETER`), fires the `os.add_dll_directory`
  audit event (PEP 578 machinery from RFC 0031/0060), calls
  `AddDllDirectory`, and returns an `_AddedDllDirectory` instance.
- `_AddedDllDirectory`: `close()` (idempotent; calls
  `RemoveDllDirectory` with the stored cookie), `__enter__`
  returning self, `__exit__` closing, and CPython's repr —
  `<AddedDllDirectory({path!r})>`, `<AddedDllDirectory()>` once
  closed. Implemented as a small native type; the cookie is a
  `DLL_DIRECTORY_COOKIE` held as a pointer-sized int.

`Win32_System_LibraryLoader` is already in the workspace
`windows-sys` feature set (RFC 0063), so no dependency motion.

### WS3 — the MSVC build surface: import library, autolink, dist

**The import library.** Linking a cdylib on `*-pc-windows-msvc`
already produces `python313.dll.lib` beside the DLL — rustc emits
it; nothing new is compiled. The dist builder learns to carry both:

- `{prefix}\python313.dll` — beside the exes at the prefix root
  (the loader's first probe, and CPython's own layout).
- `{prefix}\libs\python313.lib` — the import library, renamed from
  rustc's `python313.dll.lib` to the name MSVC's `/DEFAULTLIB`
  and setuptools' `library_dirs` convention expect. setuptools
  computes `{sys.base_exec_prefix}\libs` on its own; shipping the
  file at that path is the entire integration.

`build_artifact` locates both next to the packaged binary (they are
siblings in `target/release/`), fails the build if the DLL is
missing on Windows (a Windows artifact without the DLL is not an
artifact), and the `weavepy` binary resolution error message grows
the `-p weavepy-pylib` build hint.

**pyconfig.h autolink.** The Windows `pyconfig.h` the stdlib
materializer writes (RFC 0062 WS2 stub, kept by RFC 0063) becomes
CPython-shaped where build systems can see it: `MS_WINDOWS`,
`Py_ENABLE_SHARED`, `MS_COREDLL`, the `Py_BUILD_CORE`-guarded

```c
#pragma comment(lib,"python313.lib")
```

autolink, and the `HAVE_DECLSPEC_DLL`/`PyMODINIT_FUNC` export
shaping stock headers key off. A `.pyd` compiled with `cl /LD
ext.c /Ipath\to\include /link /LIBPATH:path\to\libs` — or through
setuptools, which passes exactly those — then binds `python313.lib`
without the build script naming it.

**The dist check cext leg un-skips on Windows.** The
`cfg!(unix)`-gated SKIP becomes a real leg: a `CEXT_SCRIPT_WINDOWS`
that discovers MSVC (in order: `cl.exe` already on `PATH`, then
`vswhere.exe` at its fixed `%ProgramFiles(x86)%\Microsoft Visual
Studio\Installer` home → newest VC tools → run the compile under
`VsDevCmd.bat -arch=x64`), compiles the same minimal module with
`cl /LD`, `/I` at the shipped `Include\`, `/LIBPATH:` at the
shipped `libs\`, names it `_weavepy_dist_cext.cp313-win_amd64.pyd`,
imports it, and calls it. No MSVC found → SKIP with the discovery
trail in the detail (truthful skip, not a silent one). The venv
leg's interpreter (a copied shim) exercises the `pyvenv.cfg`
`home=` DLL probe by construction, so the WS1 fallback is covered
by the existing matrix without a new leg.

**sysconfig residuals.** `EXT_SUFFIX`, `EXE`, and `VERSION` landed in
RFC 0063 WS6. `get_platform() == "win-amd64"` lands *here*: the frozen
`sysconfig` sniffs `'amd64' in sys.version.lower()` (CPython's own
detection), so `sys.version`'s compiler bracket gains CPython's NT
arch tag (`[WeavePy 64 bit (AMD64)]`) on Windows — without it the
platform read as `win32` and setuptools would tag wheels wrong.
No new config vars: CPython's NT `sysconfig` table carries no
`LIBRARY`/`LDLIBRARY`/`Py_ENABLE_SHARED` (those are POSIX Makefile
surface — `_init_non_posix` plus the native `_sysconfig` module
never emit them), and build tools locate the import library by the
`{sys.base_exec_prefix}\libs` convention instead. Adding them would
be a divergence, not a compatibility win. `INCLUDEPY`'s NT value
keeps pointing at the artifact `Include\` from RFC 0063.

### WS4 — `_WindowsConsoleIO`: the deferred console-Unicode story

A new Windows-gated raw-io type on `_io`, following the RFC 0063
module pattern (`io_full::build` inserts it next to `FileIO`;
absent on POSIX exactly as CPython's `_io` omits it there):

- **Construction** from a CRT fd or a console path
  (`CONIN$`/`CONOUT$`/`CON`), deciding readable/writable from
  `GetConsoleMode` on the underlying handle; non-console handles
  raise `ValueError` like CPython.
- **`read`/`readinto`/`readall`**: `ReadConsoleW` into a wchar
  buffer, transcoded with `WideCharToMultiByte(CP_UTF8)`; a
  leading `\x1a` (Ctrl-Z) at the start of a read is EOF; a read
  interrupted by Ctrl-C surfaces `ERROR_OPERATION_ABORTED`, which
  maps to the RFC 0063 signal trip → `KeyboardInterrupt` after the
  handler runs (the eval-breaker check the dispatcher already
  performs).
- **`write`**: `MultiByteToWideChar(CP_UTF8)` then `WriteConsoleW`,
  chunked at CPython's 32766-wchar ceiling; partial-write
  accounting returns the consumed *byte* count of whole characters,
  per winconsoleio.c.
- **Surface**: `fileno()`, `isatty()` (always `True`),
  `readable()`/`writable()`/`seekable()` (`False`), `close()`
  through the CRT fd owner from RFC 0063 WS1, `name`, `mode`.

**stdio wiring.** WeavePy's std streams are a monolithic native
`PyFile` (not CPython's three-layer stack), a documented
architectural divergence that RFC 0050/0053 built the WTF-8 stdio
contract on. This wave keeps the monolith and reroutes its byte
transport: at stream-construction time on Windows, if the CRT fd is
a real console (`GetConsoleMode` succeeds), the `PyFile` backend
reads/writes through the same `ReadConsoleW`/`WriteConsoleW` bridge
`_WindowsConsoleIO` uses, so interactive I/O round-trips the full
Unicode range regardless of the console codepage — CPython-faithful
*behavior* through WeavePy-shaped plumbing. Redirected/piped
streams (everything CI sees) keep the RFC 0063 CRT-fd path
unchanged. `sys.stdin.isatty()` etc. already answer correctly via
`_isatty`.

### WS5 — verification: what blocks now, what the flip measures later

**Blocking, this wave, on `windows-latest` `cargo test`** (the
job builds `-p weavepy-pylib` before testing so the DLL exists in
`target/debug/`):

1. `weavepy-cli/tests/windows_dll.rs` (Windows-gated):
   - the DLL loads from `target/debug/python313.dll`;
   - `GetProcAddress` resolves `weavepy_main`, `Py_Main`,
     `Py_BytesMain`, and a curated ~30-symbol C-API sample
     spanning the export families (`PyLong_FromLong`,
     `PyModule_Create2`, `PyErr_SetString`, `PyType_FromSpec`,
     `_Py_NoneStruct`, `PyCapsule_New`, …) — the smoke half of
     the POSIX `force_link_completeness` contract;
   - the shim exe runs Python through the DLL:
     `weavepy.exe -c "import sys; assert sys.dllhandle != 0"`,
     plus an `os.add_dll_directory` round-trip (add → repr →
     close → closed repr) and the `ImportError` message-shape
     probe against a nonexistent `.pyd`.
2. The RFC 0063 integration battery keeps passing (the driver
   moved crates; its behavior must not).

**Blocking, this wave, on macOS/ubuntu**: everything already
blocking, unchanged — regrtest `--check` `unexpected 0`, ecosystem
`--check` all rows, bench gate, `weavepy-dist check` all legs,
`cargo test --workspace` (which now also compiles `weavepy-pylib`
everywhere, keeping the cdylib honest on all three OSes).

**Advisory, unchanged mechanism**: the Windows regrtest/ecosystem/
bench/dist-check lanes keep running and uploading measured
artifacts. The dist-check lane now exercises the DLL layout and
(runner images carry MSVC) the un-skipped cext leg. The ecosystem
lane's numpy/pandas rows become *mechanically possible* on Windows
for the first time; their first measured results ride the existing
artifact upload. The flip-to-blocking commit — transplanting
measured `status_windows` rows, `bench-windows-x86_64.json`, and
`measured_os += ["windows"]` — remains the named first follow-up,
exactly as RFC 0063 WS7 specified; this wave adds no new flip
mechanism because RFC 0063 already landed it.

### Non-goals

- **Shipping a POSIX shared library.** `libpython313.so`/`.dylib`
  builds as a side effect of the crate split but is not packaged;
  the POSIX artifact, exe, and export story are unchanged. A
  `python3-config`/embedding wave can pick it up later (RFC 0062
  future work).
- **`python313_d.dll` debug builds, `pythonw.exe`** (the
  GUI-subsystem exe), the `py.exe` launcher, MSI/Store packaging,
  and code signing.
- **ARM64 Windows** — same posture as RFC 0063 (builds, ctypes
  `SUPPORTED=false`, no lanes).
- **Flipping the Windows lanes to blocking in this commit** — the
  flip requires CI-measured artifacts by definition (RFC 0063 WS7);
  this wave widens what those artifacts measure.
- **The stable ABI's version-crossing promises** (`python3.dll`
  forwarding for abi3 wheels built against other minors): abi3
  wheels tagged for 3.13 load through `python313.dll` like any
  other; a `python3.dll` forwarder DLL is deferred until a concrete
  consumer demands it.

### Acceptance criteria

1. **The restructure is invisible on POSIX**: `cargo fmt`,
   `clippy -D warnings`, `cargo test --workspace`, regrtest
   `--check` (`unexpected 0`), ecosystem `--check` (all rows),
   bench gate, and `weavepy-dist check` (all 7 legs) green on
   macOS, with the ubuntu twins green in CI.
2. **Windows compiles clean from the cross-check**:
   `cargo check --target x86_64-pc-windows-msvc --workspace`
   passes locally (no linking; the link is proven on the runner).
3. **The DLL is real and complete**: the `windows-latest` test job
   loads `python313.dll`, resolves the entry points and the C-API
   symbol sample, and runs Python end-to-end through the shim —
   all blocking.
4. **`sys.dllhandle` is truthful**, `os.add_dll_directory` matches
   CPython's surface (context manager, repr, audit event, absolute-
   path validation), and extension-load failures raise CPython's
   `ImportError` shape on Windows.
5. **The artifact carries the ABI**: `weavepy-dist build` on
   Windows places `python313.dll` at the prefix root and
   `libs\python313.lib` beside `Include\`; `weavepy-dist check`
   passes with the cext leg PASS (MSVC present) or a truthful
   discovery-trail SKIP — no unconditional SKIP remains.
6. **Console Unicode round-trips**: the `_WindowsConsoleIO` type
   registers on `_io` (Windows), the console-backed stdio bridge
   passes its Windows-gated unit tests (UTF-8 supplementary-plane
   round-trip through `WriteConsoleW`/`ReadConsoleW` mocks at the
   CRT layer where a real console is absent in CI, plus behavior
   tests under a real console handle when available).
7. **The RFC 0063 battery keeps passing unmodified** — the driver
   relocation is behavior-neutral.

## Drawbacks

- **The exe/DLL split is a second distribution identity to keep
  honest.** A version-skewed pair (old exe, new DLL) is a new
  failure class that the static exe could not have. Mitigation: the
  shim and DLL ship from one build; the shim's failure message
  names both paths; `weavepy-dist check`'s identity leg runs
  through the shim and would surface skew as a version mismatch.
- **`GetProcAddress`-based dispatch hides link errors until
  runtime.** A typo'd entry-point name fails at shim startup, not
  at build. Mitigation: the blocking Windows test calls the real
  entry points on every PR.
- **rustc's cdylib export behavior is now load-bearing.** If a
  future rustc narrows default exports (e.g. under fat LTO), the
  DLL could silently thin. Mitigation: the ~30-symbol
  `GetProcAddress` sample in the blocking test turns "silently
  thin" into "red PR"; the `FORCE_LINK` table keeps reachability
  explicit.
- **The console bridge adds a third stdio transport** (POSIX fd,
  NT CRT fd, NT console-W). The monolithic `PyFile` keeps the
  surface area contained, but it is one more arm in every stdio
  bugfix.
- **Iteration on Windows-only failures is still a CI round-trip**
  (RFC 0063's standing drawback). The cross-check target and
  fine-grained test battery are the standing mitigation.

## Alternatives

- **Export the C-API from the exe and ship a forwarder/stub
  `python313.dll`** (trampolines resolved via
  `GetProcAddress(GetModuleHandle(NULL))` at `DllMain`): keeps the
  static exe, but PE export forwarders cannot target an exe by
  name, so every one of ~682 symbols needs a generated jump thunk;
  the DLL would be unloadable outside a WeavePy process (embedders,
  tools that `LoadLibrary` the Python DLL directly); and the exe
  needs 682 `/EXPORT` args anyway. Strictly more machinery than
  moving the code into the DLL, with a worse compatibility story.
- **Link the shim against the import library at build time**
  (CPython's literal shape) instead of `LoadLibraryW` at startup:
  cargo cannot express "bin links a sibling crate's cdylib
  artifact" on stable (artifact dependencies are unstable), so the
  link would need a build.rs racing the cdylib's build for the
  `.lib`'s existence. Runtime loading is order-independent, keeps
  `cargo build --workspace` correct by construction, and costs one
  `LoadLibrary` + `GetProcAddress` at startup.
- **Let the Windows bin keep its static runtime and also ship the
  DLL**: two copies of the interpreter in one process (the exe's
  dead static copy plus the DLL the `.pyd`s bind), ~4× artifact
  bloat across the four exe copies, and a state-split footgun if
  any exe-side code ever runs. Rejected outright.
- **Copy `python313.dll` into every venv's `Scripts\`** instead of
  teaching the shim `pyvenv.cfg`: burns ~35 MB per venv and leaves
  stale-DLL venvs behind on upgrade; the `home=` probe is four
  lines and matches how the landmark walk already resolves venvs.
- **Build `_WindowsConsoleIO` as the full CPython three-layer stdio
  stack** (raw + buffered + text wrapper for std streams): the
  faithful shape, but it would rebuild WeavePy's stdio architecture
  in one wave for no user-visible delta over rerouting the
  monolith's transport; the RFC 0050 WTF-8 contract tests pin the
  observable behavior either way. Deferred, not rejected —
  revisited if `sys.stdout.buffer.raw`-introspecting code appears
  in the ecosystem lane.

## Prior art

- **CPython** (`PC/`, `Programs/python.c`, `Python/dynload_win.c`):
  the shim-exe + core-DLL split, the `libs\` import-library
  convention, the autolink pragma, and the `LoadLibraryExW` flag
  set are transcribed, not invented.
- **python-build-standalone**: ships exactly this shape
  (`python.exe` + `python313.dll` + `libs\python313.lib`) built
  outside MSBuild, and its venvs rely on `pyvenv.cfg` `home=` —
  the direct precedent for the shim's probe order.
- **PyPy on Windows**: `libpypy3.9-c.dll` beside a thin exe;
  its documented lesson (extensions and embedders need the DLL's
  directory discoverable) shaped the exe-dir-first probe.
- **Rust cdylib C-API precedents**: the `#[no_mangle]`-graph export
  model is how PyO3's `abi3` builds and wasm component crates ship
  multi-crate C surfaces; the `FORCE_LINK` table predates this wave
  (RFC 0022) and was designed for exactly this reuse.
- **RustPython**: still exe-only on Windows and cannot load CPython
  wheels — the counterexample this wave graduates past.

## Unresolved questions

- Whether `Py_Main`/`Py_BytesMain` should tear down and return
  (CPython returns the exit code and the caller may re-init) or
  behave like `weavepy_main` (single-shot). This wave implements
  return-the-code without re-init support, matching WeavePy's
  existing `Py_Finalize` no-op posture; revisit with the embedding
  wave.
- Whether the ecosystem lane's Windows wheel fetch should pin
  `win_amd64` wheels in `tools/ecosystem_fetch.py` now or at the
  flip commit — answered at the flip, when the lane's first
  measured numpy/pandas rows exist.
- Whether `ctypes.CDLL(sys.executable)`-style self-loads (rare, but
  real) need the exe to re-export anything. Believed no (consumers
  use `sys.dllhandle`); the flip's measured `test_ctypes` rows will
  answer.

## Future work

- The flip-to-blocking baseline commit (measured `status_windows`
  rows including the cext-dependent files, ecosystem
  `status_windows` for the binary-wheel rows,
  `bench-windows-x86_64.json`, `measured_os += ["windows"]`) —
  first follow-up, unchanged from RFC 0063's naming.
- A `python3.dll` stable-ABI forwarder once an abi3 consumer that
  needs it appears in the ecosystem lane.
- `pythonw.exe` (GUI subsystem) and `venvlauncher`-style script
  shims.
- Packaging the POSIX shared library + `python3-config` (RFC 0062
  future work, now cheaper: the cdylib exists).
- The full three-layer stdio stack if `buffer`/`raw` introspection
  surfaces as a real-world blocker.

## Results

*(To be filled in at landing, per repo convention: the Windows CI
battery outcomes, the first advisory-lane sweeps over the DLL
layout, and the unchanged macOS/Linux baselines.)*
