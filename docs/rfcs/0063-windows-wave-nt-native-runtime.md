# RFC 0063: The Windows wave — NT-native runtime core, CRT fd model, IOCP asyncio, and measured Windows baselines

- **Status**: Accepted
- **Authors**: WeavePy authors
- **Created**: 2026-08-11
- **Tracking issue**: TBD
- **Builds on**: RFC 0062 (per-OS expectation keys, per-platform bench
  baselines, the dist builder and check matrix this wave extends to a
  zip artifact), RFC 0053 (the landmark-walk prefix discovery that
  already special-cases `%LOCALAPPDATA%` and `;`-separated
  `PYTHONHOME`), RFC 0040/0042 (the systems/networking stdlib whose
  POSIX bodies gain NT twins), RFC 0026 (multiprocessing, whose
  Windows frozen files have been shipped-but-dead since), RFC 0054
  (asyncio, whose `windows_events`/`windows_utils` modules await
  `_overlapped`), RFC 0022/0043 (the C-API/FFI layer whose ctypes
  call gate gains the Win64 ABI).

## Summary

WeavePy claims "drop-in replacement" and backs it with measured
baselines — on exactly two platforms. On Windows, the platform with
the largest desktop Python install base, WeavePy today is a binary
that compiles and passes `cargo test`, and nothing else: `os.open`
raises `NotImplementedError`, `select.select` raises `OSError`,
Ctrl-C is never delivered, `fileno()` would return a raw `HANDLE`
where every consumer expects a CRT fd, and the four native modules
the frozen Windows stdlib already imports — `_winapi`, `msvcrt`,
`winreg`, `_overlapped` — do not exist. This wave lands the NT-native
runtime core: a CRT-fd model matching CPython's (`_open_osfhandle` /
`_get_osfhandle` at the io/mmap/socket boundaries), a
`winerror`-truthful `OSError` taxonomy, the `_winapi` + `msvcrt` +
`winreg` + `_overlapped` quartet as real Rust modules over
`windows-sys`, Winsock `select`, the Win64 ctypes call gate,
`mbcs`/`oem` codecs, Ctrl-C via `SetConsoleCtrlHandler`, spawn-method
multiprocessing over named pipes, and IOCP-backed proactor asyncio.
Distribution follows: the dist builder grows a zip format and a
CPython-shaped Windows layout (`python.exe` at the prefix root,
`Scripts\` venvs), and CI grows Windows regrtest / ecosystem / bench
/ dist lanes that upload measured artifacts, advisory until their
first measured baselines are committed — the same
mechanism-first-then-measurement discipline RFC 0062 used for Linux
bench. The blocking gate that lands *with* the wave is the existing
`windows-latest` `cargo test` job, grown with Windows-gated
integration tests that boot the interpreter and exercise every new
module for real.

## Motivation

1. **The claim is untested where most users are.** Every measured
   number in the README — 515/548 regrtest, 31/31 ecosystem — is a
   macOS/Linux number. Python's desktop install base is majority
   Windows; a "drop-in replacement" that cannot open a file there is
   not one. The cost of inaction compounds: every wave that lands
   POSIX-only deepens the `libc::` monoculture (233 call sites in
   `os.rs` alone) and makes the eventual port more invasive.

2. **The pure-Python half is already shipped and dead.** The frozen
   tree embeds `ntpath`, `asyncio/windows_events.py`,
   `multiprocessing/popen_spawn_win32.py`, `ctypes/wintypes.py`, the
   sysconfig `nt`/`nt_venv` schemes, and the `pathlib` Windows
   classes unconditionally — RFC 0026 and 0054 froze them "for
   completeness but never imported on POSIX". They import `_winapi`,
   `msvcrt`, and `_overlapped`, none of which exist. The marginal
   cost of making Windows real is concentrated in the native layer,
   because the Python layer was paid for in prior waves.

3. **The foundations landed one wave ago.** RFC 0062 shipped per-OS
   expectation keys (`status_windows` — mechanism live, zero rows
   using it), per-platform bench baselines with an
   advisory-when-missing gate, `.exe`-aware artifact naming, and
   `;`-separated `PYTHONHOME`. RFC 0053's landmark walk already picks
   `%LOCALAPPDATA%\weavepy` as the Windows cache root. The port has a
   prepared slot to land in.

4. **Half-support is worse than none.** Today `import fcntl` succeeds
   on a Windows build and returns stubs that fail at call time
   (CPython: the import fails, and portable code keys off that);
   `sys.builtin_module_names` advertises modules that don't exist;
   `OSError` has a `winerror` slot that is never populated. Portable
   code that correctly branches on documented signals gets the wrong
   branch. Making the signals truthful is itself a compatibility fix.

## CPython reference

- **fd model**: CPython on Windows works in CRT file descriptors
  everywhere Python-visible (`Modules/posixmodule.c` uses `_wopen`,
  `_read`, `_write`, `_close`; `PC/msvcrtmodule.c` exposes
  `get_osfhandle`/`open_osfhandle` as the fd↔HANDLE bridge).
  `socket.fileno()` is the one exception: it returns the `SOCKET`
  (an unrelated kernel-handle namespace), and `select.select` /
  `_overlapped` consume SOCKETs, not fds.
- **`_winapi`**: `Modules/_winapi.c` — the private Win32 surface
  `subprocess`, `multiprocessing`, and `shutil` consume:
  `CreateProcess`, `CreatePipe`, `CreateNamedPipe`/`ConnectNamedPipe`,
  `CreateFile`, `CreateJunction`, `DuplicateHandle`,
  `WaitForSingleObject`/`WaitForMultipleObjects`, `CreateEventW`,
  `CreateMutexW`, `OpenProcess`, `TerminateProcess`,
  `GetExitCodeProcess`, `GetStdHandle`, `ReadFile`/`WriteFile`,
  `PeekNamedPipe`, `GetLastError`, the `STARTF_*`/`CREATE_*`
  constant families, and the `Overlapped` helper type.
- **`msvcrt`**: `PC/msvcrtmodule.c` — `get_osfhandle`,
  `open_osfhandle`, `setmode`, `locking`, `get_error_mode`,
  `CrtSetReportMode`, console I/O (`kbhit`, `getch`/`getwch`,
  `putch`/`putwch`, `ungetch`).
- **`winreg`**: `PC/winreg.c` — `OpenKey`/`CreateKey(Ex)`,
  `EnumKey`/`EnumValue`, `QueryValueEx`/`SetValueEx`,
  `DeleteKey(Ex)`/`DeleteValue`, `QueryInfoKey`, `ConnectRegistry`,
  the `PyHKEY` handle type with context-manager semantics, and the
  `REG_*`/`KEY_*`/`HKEY_*` constants; value round-trips per type
  (`REG_SZ`/`EXPAND_SZ` UTF-16, `REG_MULTI_SZ` list-of-str,
  `REG_DWORD`/`QWORD`, `REG_BINARY` bytes).
- **`_overlapped`**: `Modules/overlapped.c` — the IOCP layer under
  `asyncio.ProactorEventLoop`: `CreateIoCompletionPort`,
  `GetQueuedCompletionStatus`, `PostQueuedCompletionStatus`,
  `CreateEvent`/`SetEvent`/`ResetEvent`,
  `RegisterWaitWithQueue`/`UnregisterWait(Ex)`, `ConnectPipe`, and
  the `Overlapped` type with `ReadFile`, `WriteFile`, `WSARecv`,
  `WSASend`, `AcceptEx`, `ConnectEx`, `TransmitFile`, `DisconnectEx`,
  `cancel`, `getresult`.
- **Errors**: `Objects/exceptions.c` `oserror_parse_args` — on
  Windows a raw Win32 error is translated to an approximate errno
  (`PC/errmap.h`, generated `winerror_to_errno`), the original code
  is preserved on `OSError.winerror`, and `strerror` comes from
  `FormatMessageW`. Winsock `WSAE*` codes ≥ 10000 pass through as
  errno values (`errno.WSAEWOULDBLOCK == 10035`).
- **Signals**: `Modules/signalmodule.c` — Windows supports the C90
  set plus `SIGBREAK`; Ctrl-C arrives via `SetConsoleCtrlHandler`
  (`CTRL_C_EVENT`/`CTRL_BREAK_EVENT`), trips the flag, and the eval
  loop raises `KeyboardInterrupt`.
- **Codecs**: `Objects/unicodeobject.c` code-page codecs —
  `mbcs` = `CP_ACP` and `oem` = `CP_OEMCP` via
  `MultiByteToWideChar`/`WideCharToMultiByte`;
  `Lib/encodings/mbcs.py` and `oem.py` are two-line shims over
  `codecs.code_page_encode/decode`.
- **Layout**: CPython Windows installs put `python.exe` at the
  prefix root, the stdlib under `Lib\`, headers under `Include\`;
  venvs use `Scripts\python.exe` (the `nt_venv` sysconfig scheme).
  `sys.getwindowsversion()`, `sys.winver`, `time.monotonic` via
  `QueryPerformanceCounter`.
- **ctypes**: `Modules/_ctypes/` — on Win64 there is one calling
  convention (the distinction between `CFUNCTYPE` and `WINFUNCTYPE`
  is vestigial); `FormatError`, `GetLastError`,
  `get_last_error`/`set_last_error` (the `use_last_error` protocol
  swaps a thread-local around the foreign call), `WinDLL`/`windll`/
  `oledll`, HRESULT checking.

## Detailed design

The wave is seven workstreams. WS1 (fd + error model) is the
foundation everything else consumes; WS2–WS5 are the native modules
in dependency order; WS6 is distribution; WS7 is measurement. The
implementation-verification channel for all of them is twofold:
`cargo build` for the `x86_64-pc-windows-msvc` target must stay
clean locally (rlib builds fully compile the Windows code without
linking), and the existing blocking `windows-latest` `cargo test` CI
job gains Windows-gated integration tests that boot the interpreter
and exercise each new surface end-to-end.

### WS1 — the NT foundation: CRT fds, winerror, signals, sys surface

**Dependency.** `weavepy-vm` gains a target-scoped dependency on
`windows-sys` (Win32 API bindings; features enumerated per use:
`Win32_Storage_FileSystem`, `Win32_System_Threading`,
`Win32_System_Pipes`, `Win32_System_IO`, `Win32_Networking_WinSock`,
`Win32_System_Registry`, `Win32_System_Console`,
`Win32_Security_Cryptography`, `Win32_Globalization`, …). CRT
functions (`_open_osfhandle`, `_get_osfhandle`, `_wopen`, `_read`,
`_write`, `_close`, `_dup`, `_dup2`, `_pipe`, `_lseeki64`,
`_chsize_s`, `_isatty`, `_setmode`, `_locking`, `_kbhit`, `_getwch`,
…) are declared as `extern "C"` imports from the UCRT the MSVC
target already links.

**The fd model — the wave's load-bearing decision.** WeavePy on
Windows adopts CPython's CRT-fd model:

- `os.open` opens via `_wopen` (UTF-16 path, `O_BINARY` implied like
  CPython, `O_NOINHERIT` for `close_on_exec` semantics) and returns
  the CRT fd. `os.read`/`os.write`/`os.close`/`os.dup`/`os.dup2`/
  `os.lseek`/`os.ftruncate`/`os.isatty`/`os.pipe` map to their CRT
  twins (`os.pipe` via `CreatePipe` + `_open_osfhandle`, matching
  CPython's non-inheritable default). The `#[cfg(not(unix))]`
  `NotImplementedError` stubs in `os.rs` are deleted, not gated.
- `FileIO` on Windows becomes fd-backed like the Unix hot path:
  where Unix `io.rs` snapshots `AsRawFd` and drains via
  `libc::read`/`libc::write` with the GIL released, Windows does the
  same over `_read`/`_write` on the CRT fd. The fd is the single
  owner; `std::fs::File` views (needed for metadata calls) are
  constructed non-owning from `_get_osfhandle` and leaked back via
  `ManuallyDrop`. `fileno()` returns the CRT fd — the
  `as_raw_handle() as i64` branch in `object.rs` is retired.
- `mmap.file_from_fileno` stops treating the int as a `HANDLE` and
  bridges via `_get_osfhandle` (the comment in `mmap_mod.rs` already
  anticipated this). `flush()` gains the `FlushViewOfFile` +
  `FlushFileBuffers` pair; `size()` gains `GetFileSizeEx`.
- Sockets keep the `SOCKET`-as-fileno model (CPython does too);
  nothing changes there.

**The error model.** `error.rs::io_error_to_py` grows the Windows
arm: `raw_os_error()` on Windows is the Win32 error; it is mapped to
an approximate errno via a generated `winerror_to_errno` table (the
`PC/errmap.h` mapping, ~120 entries, transcribed as a Rust match),
the original code is stored on `OSError.winerror`, `strerror` comes
from `FormatMessageW` (trailing CRLF trimmed, like CPython), and the
PEP 3151 subclass is chosen from the *mapped errno* so
`FileNotFoundError`/`PermissionError`/… taxonomy holds. Winsock
errors (`WSAE*`) pass through as errno values ≥ 10000. The
`errno` module gains the `WSAE*` constant family on all platforms
(CPython ships them Windows-only; WeavePy gates them the same way).
The half-wired `winerror` slot in `builtin_types.rs` (constructor
currently forces `None`) is completed: the 4-arg `OSError`
constructor form performs the winerror→errno mapping exactly like
`oserror_parse_args`.

**Signals.** `signal_mod.rs`'s Windows no-ops become real:
`install_startup_dispositions` registers a `SetConsoleCtrlHandler`
trampoline mapping `CTRL_C_EVENT`→`SIGINT` and
`CTRL_BREAK_EVENT`→`SIGBREAK` onto the existing atomic trip +
wakeup mechanism (the wakeup write goes through the CRT fd or
socket per `signal.set_wakeup_fd` semantics); `raise_signal` calls
CRT `raise`; `SIGBREAK` joins the constant set; `set_os_disposition`
installs CRT `signal()` handlers for the C90 set so
`signal.SIG_IGN` semantics hold.

**sys/os surface.** `sys.getwindowsversion()` (a structseq over
`RtlGetVersion`, with `platform_version` from the same source),
`sys.winver = "3.13"`, `sys.dllhandle = 0` (no python DLL — see
Non-goals), `sys._enablelegacywindowsfsencoding` as a no-op
(filesystem encoding is always UTF-8, matching PEP 529 defaults).
`os` gains the NT-only names portable code probes for:
`os.startfile` (ShellExecuteW), `os.get_terminal_size` via console
API, `os.getlogin` via `GetUserNameW`, `os.urandom` via
`BCryptGenRandom` (replacing any `/dev/urandom` assumption),
`os.cpu_count` via `GetActiveProcessorCount`, `O_BINARY`/`O_TEXT`/
`O_NOINHERIT`/`O_TEMPORARY`/`O_SHORT_LIVED`/`O_SEQUENTIAL`/
`O_RANDOM` constants, and the `nt._path_splitroot_ex` /
`nt._path_normpath` fast paths `ntpath` probes (fallbacks exist, so
these are speed, not correctness). `os.environ` becomes
case-insensitive-key on Windows at the Rust layer (CPython upcases
in `nt`); `os.listdir`/`scandir`/`stat` already route through
`std::fs` and inherit Windows support, but `stat` results gain
`st_file_attributes` and the reparse-point `st_mode` shaping CPython
applies.

**Truthful inventories.** `fcntl` registration moves behind
`#[cfg(unix)]` (joining `termios`/`resource`); `_posixsubprocess`
and `_posixshmem` likewise. `sys.builtin_module_names` is rebuilt
from the actual registration table at `register_all` time instead
of the stale hardcoded tuple, so the Windows build advertises
`_winapi`/`msvcrt`/`winreg`/`nt` (as CPython does) and never
advertises absent POSIX modules. The frozen `nt_mod.py` shim stays
(the architecture keeps Rust-`os`-as-owner) but its stub surface
(`_getdiskusage` via `GetDiskFreeSpaceExW`,
`_supports_virtual_terminal` via console-mode probe) becomes real.

### WS2 — `_winapi` and `msvcrt`: the process/handle core

`crates/weavepy-vm/src/stdlib/winapi_mod.rs`, registered as
`_winapi` under `#[cfg(windows)]`. The full CPython 3.13 surface the
frozen stdlib consumes:

- Process: `CreateProcess` (UTF-16 command line, environment block
  construction with the sorted-uppercase-key contract,
  `STARTUPINFOW` incl. `hStdInput`/`hStdOutput`/`hStdError` and
  `lpAttributeList` handle lists), `OpenProcess`,
  `TerminateProcess`, `GetExitCodeProcess`, `GetCurrentProcess`,
  `ExitProcess`, `GetModuleFileName`.
- Handles: a `HANDLE`-wrapping int subclass with `Close()`/
  `Detach()` (CPython's `_winapi` returns plain ints from most APIs
  and the `Handle` class from `CreatePipe` consumers in
  `subprocess`; WeavePy matches the observable shapes),
  `DuplicateHandle`, `CloseHandle`, `GetStdHandle`,
  `SetStdHandle`, `GetHandleInformation`/`SetHandleInformation`.
- Pipes and files: `CreatePipe`, `CreateNamedPipe`,
  `ConnectNamedPipe` (sync + overlapped), `WaitNamedPipe`,
  `PeekNamedPipe`, `SetNamedPipeHandleState`, `CreateFile`,
  `ReadFile`/`WriteFile` (sync + overlapped via the module's own
  `Overlapped` helper), `CreateJunction` (reparse-point write, used
  by `test_os`/pip).
- Sync: `WaitForSingleObject`, `WaitForMultipleObjects`,
  `CreateEventW`/`OpenEventW`/`SetEvent`/`ResetEvent`,
  `CreateMutexW`/`OpenMutexW`/`ReleaseMutex`, `CreateFileMapping`/
  `OpenFileMapping`/`MapViewOfFile`/`UnmapViewOfFile`/
  `VirtualQuerySize` (the `_multiprocessing.shared_memory` NT
  backend rides these).
- Misc: `GetLastError`, `GetACP`, `GetFileType`,
  `GetVersion`, `NeedCurrentDirectoryForExePath`,
  `CopyFile2` (used by `shutil`'s fast copy path — un-stubbing the
  `_winapi = None` patch in the frozen `shutil.py`), `LCMapStringEx`
  (`ntpath.normcase` fast path), and the full constant family
  (`STARTF_*`, `CREATE_*`, `DUPLICATE_*`, `FILE_*`, `PIPE_*`,
  `WAIT_*`, `INFINITE`, `NULL`, `SW_HIDE`, …).

All blocking waits (`WaitFor*`, `ConnectNamedPipe`, blocking
`ReadFile`/`WriteFile`) release the GIL through the same
`blocking_region` mechanism the socket layer uses.

`crates/weavepy-vm/src/stdlib/msvcrt_mod.rs`, registered as
`msvcrt`: `get_osfhandle`/`open_osfhandle` (the WS1 bridge, exposed),
`setmode`, `locking` (+ `LK_*` constants), `get_error_mode`/
`SetErrorMode` constants, `heapmin`, and the console family
(`kbhit`, `getch`/`getche`/`getwch`/`getwche`, `putch`/`putwch`,
`ungetch`/`ungetwch`) over the console CRT.

**Subprocess.** The frozen `subprocess.py` gains its Windows arm:
`_mswindows` path drives `_winapi.CreateProcess` with
`STARTUPINFO`, handle inheritance lists, `CREATE_NEW_CONSOLE`/
`CREATE_NEW_PROCESS_GROUP` flags, and `Handle.wait` via
`_winapi.WaitForSingleObject` — replacing the portable
`_subprocess.spawn` fallback on Windows (which stays as the
non-POSIX-non-NT fallback). `Popen.send_signal(CTRL_BREAK_EVENT)`
works via `GenerateConsoleCtrlEvent`.

**Multiprocessing.** `_multiprocessing`'s `#[cfg(unix)]` SemLock
gains an NT twin over `CreateSemaphoreW`/`ReleaseSemaphore`/
`WaitForSingleObjectEx` (recursive-mutex emulation identical to
CPython's `win32` branch in `semaphore.c`, including
`WaitForMultipleObjects` on the sigint event for main-thread
acquires). The frozen `connection.py`/`reduction.py`/
`popen_spawn_win32.py` Windows branches — already shipped — start
importing cleanly against the real `_winapi` + `msvcrt`; `Pipe()`
on NT uses `CreateNamedPipe` per CPython. `spawn` becomes the
default and only start method on Windows (as in CPython);
`_posixshmem` stays POSIX-gated with shared memory on NT routed
through `_winapi.CreateFileMapping`.

### WS3 — `winreg`, codecs, and platform identity

`crates/weavepy-vm/src/stdlib/winreg_mod.rs`, registered as
`winreg`: the `PyHKEY` type (int-comparable, context manager,
`Close`/`Detach`, `__bool__`), the full function surface
(`OpenKey(Ex)`, `CreateKey(Ex)`, `DeleteKey(Ex)`, `DeleteValue`,
`EnumKey`, `EnumValue`, `QueryInfoKey`, `QueryValue(Ex)`,
`SetValue(Ex)`, `ConnectRegistry`, `FlushKey`, `LoadKey`, `SaveKey`,
`Disable/Enable/QueryReflectionKey`, `ExpandEnvironmentStrings`),
value marshalling for every `REG_*` type per CPython's `Reg2Py`/
`Py2Reg` (UTF-16 strings, `REG_MULTI_SZ` double-NUL lists,
`REG_DWORD`/`QWORD` little-endian ints, `None`→`REG_NONE`), and the
`HKEY_*`/`KEY_*`/`REG_*` constants. `platform.win32_ver()` then
works out of the box through its existing winreg fallback (no `_wmi`
— see Non-goals).

**Codecs.** Native `codecs.code_page_encode`/`code_page_decode` over
`MultiByteToWideChar`/`WideCharToMultiByte` (with the exact CPython
error-handler contract: `strict` surfaces `UnicodeEncodeError` with
the failing span; `replace` uses the API's default-char path), plus
`mbcs_encode`/`mbcs_decode` (CP_ACP) and the `oem` pair (CP_OEMCP).
The frozen `encodings/mbcs.py` and `encodings/oem.py` are adopted
verbatim from CPython 3.13 and registered (import-gated on win32
like CPython's package does naturally via the codec search
function). The `cp932`/`cp949`/`cp950` CJK pages already have native
tables from RFC 0050's CJK work and need only alias wiring.

### WS4 — sockets, `select`, and IOCP asyncio

**Winsock init.** `_socket` import on Windows performs `WSAStartup`
once (module-level, like CPython), not lazily inside `getservbyname`.

**`select.select` on Windows.** The non-unix stub is replaced by a
real Winsock `select()` over `fd_set`s built from SOCKETs (with
CPython's semantics: non-socket values raise, empty-lists +
timeout sleeps, `[], [], []` on timeout). `selectors.DefaultSelector`
then resolves to `SelectSelector` exactly as CPython does on
Windows. `select.poll`/`epoll`/`kqueue` stay absent on NT (truthful
`hasattr` signals).

**Socket residuals.** The `accept`-timeout wait (a `libc::poll` path
today) gains a Winsock twin (`select` on the one SOCKET);
`getaddrinfo`/`getnameinfo` route through Winsock's own
`GetAddrInfoW`/`GetNameInfoW` (replacing the `ToSocketAddrs`
approximation, restoring `AI_PASSIVE`/`AI_CANONNAME` fidelity);
inheritable get/set via `SetHandleInformation`; `socket.socketpair`
comes from the frozen `socket.py` loopback emulation CPython also
uses. Winsock call failures map through `WSAGetLastError` into the
WS1 error model.

**`_overlapped`.** `crates/weavepy-vm/src/stdlib/overlapped_mod.rs`,
registered under `#[cfg(windows)]`: `CreateIoCompletionPort`,
`GetQueuedCompletionStatus` (GIL-released), 
`PostQueuedCompletionStatus`, `CreateEvent`/`SetEvent`/`ResetEvent`,
`RegisterWaitWithQueue`/`UnregisterWait(Ex)` (thread-pool wait
packets), `BindLocal`, `WSAConnect`, and the `Overlapped` type with
its full method set — `ReadFile`/`ReadFileInto`, `WriteFile`,
`WSARecv`/`WSARecvInto`, `WSASend`, `AcceptEx` (+
`GetAcceptExSockaddrs` address parsing), `ConnectEx`,
`DisconnectEx`, `TransmitFile`, `ConnectNamedPipe`, `cancel`,
`getresult(wait)`, `pending`/`address`/`error` — the extension
functions loaded once via `WSAIoctl(SIO_GET_EXTENSION_FUNCTION_
POINTER)` per CPython. Buffer ownership follows CPython's rule: the
`Overlapped` object pins its buffer until completion or cancellation
drain, so the VM never frees memory the kernel still owns.

With `_overlapped`, `_winapi`, and `msvcrt` live, the frozen
`asyncio/windows_events.py` + `windows_utils.py` import cleanly and
`ProactorEventLoop` becomes the Windows default policy exactly as
frozen; `SelectorEventLoop` works over WS4's `select` as the
alternative, and asyncio subprocess support rides the WS2
`subprocess` arm.

### WS5 — ctypes: the Win64 call gate

`ctypes_native/ffi/native.rs` grows the `windows + x86_64` arm:
`SUPPORTED = true`, a Win64-ABI call gate (RCX/RDX/R8/R9 + XMM0–3
with the shadow-space and by-reference-aggregate rules — one
convention, so `FUNCFLAG_STDCALL` is accepted and ignored as on
CPython Win64), and closure trampolines for callbacks. The loader
goes wide (`LoadLibraryW`, default `LOAD_WITH_ALTERED_SEARCH_PATH`
semantics matching CPython's `CDLL(winmode=...)` default);
`last_dlerror` uses `FormatMessageW`. The frozen `_ctypes` gains
`FormatError`, `get_last_error`/`set_last_error` backed by a native
thread-local that the call gate swaps around foreign calls when
`use_last_error=True`, `_check_HRESULT`, and `CopyComPointer` as a
stub. `ctypes.wintypes`, `WinDLL`/`windll`/`oledll`, and `WinError`
already exist in the frozen layer and light up. aarch64-windows is
explicitly out (build works, `SUPPORTED=false`, like today's
non-x86_64 story).

### WS6 — distribution: the zip artifact and the NT prefix

**Layout.** The Windows artifact adopts the CPython convention at
the root while keeping WeavePy's landmark:

```text
weavepy-<version>+g<sha>-x86_64-pc-windows-msvc/
├── python.exe               # copies of the release binary
├── python3.exe
├── weavepy.exe
├── lib/
│   └── weavepy3.13/         # the landmark tree (unchanged name —
│       ├── .weavepy-complete #  the walk finds it from the exe's
│       └── site-packages/    #  own directory = the prefix)
├── include/
│   └── python3.13/
├── README.md
└── LICENSE-{APACHE,MIT}
```

The exe sits at the prefix root (so `resolve()`'s ancestor walk
finds `{prefix}/lib/weavepy3.13` from the first parent — no code
change needed), there is no `bin/`, and no symlinks exist anywhere
in the artifact. `weavepy-dist` gains `--format zip` (default on
Windows; `tar -a -cf` — bsdtar ships on the GitHub runners and
autodetects zip from the extension) and the `check` matrix learns
the NT shape: `python3.exe` at the root, venv leg at
`venv\Scripts\python.exe`, cext leg SKIP (see Non-goals), all other
legs identical.

**Venv.** The frozen `venv` package's Windows branch expects
`venvlauncher.exe` assets CPython builds; WeavePy patches
`setup_python`'s win32 arm to copy `sys._base_executable` itself as
`Scripts\python.exe` (the python-build-standalone approach — a real
copy, no launcher indirection, works because the landmark walk
chases `pyvenv.cfg` `home=` already). `DATA_FILES` gains
`venv/scripts/nt/activate.bat` + `deactivate.bat` (adopted from
CPython 3.13; `Activate.ps1` is already in `common`).

**sysconfig.** The frozen `nt`/`nt_venv` schemes get
WeavePy-truthful paths (`stdlib`/`platstdlib` →
`{installed_base}/lib/weavepy3.13`, `purelib`/`platlib` →
`{base}/lib/weavepy3.13/site-packages` for the prefix scheme and
`{base}/Lib/site-packages` for venvs per CPython, `scripts` →
`{base}/Scripts`, `include` → `{installed_base}/include/python3.13`)
— the same divergence-with-documentation policy the POSIX scheme
took in RFC 0053. `sysconfig_native` already reports
`EXT_SUFFIX=.cp313-win_amd64.pyd`; it gains truthful `nt` values for
the query surface pip touches (`get_platform() == "win-amd64"`,
`VERSION_NODOT`, `EXE=".exe"`). The materializer writes the stub
`pyconfig.h` on Windows still (C builds are out — Non-goals) and
skips the POSIX `lib/python3.13` symlink.

### WS7 — measurement: CI lanes and the advisory-until-measured gate

**The blocking gate that lands with the wave**: the existing
`windows-latest` `test` job, grown with Windows-gated integration
tests under `crates/weavepy/tests/` (running Python source through
the embedding API) and per-module Rust unit tests: CRT fd round-trip
(`os.pipe`→`os.write`→`os.read`→`msvcrt.get_osfhandle`), `_winapi`
anonymous + named pipe echo through `CreateProcess` of the test
binary itself, `winreg` HKCU round-trip under a scratch subkey
(created and deleted per test), `select.select` over a loopback
socket pair, `_overlapped` IOCP read/write completion against
loopback sockets and `ConnectNamedPipe`, ctypes calling
`kernel32.GetTickCount64` + a callback trampoline, `mbcs` codec
round-trips, `subprocess.run` capture, a `multiprocessing` spawn
Pool map, and an asyncio proactor echo server. These are real
Windows executions, gating every PR, from this wave forward.

**New CI lanes (advisory)**: `regrtest`, `ecosystem` (wheels fetched
on the runner), `bench`, and `dist-check` each gain a
`windows-latest` matrix leg. Regrtest and ecosystem gain the
mechanism RFC 0062's bench lane already has: the expectations files
carry a `measured_os = ["macos", "linux"]` stamp; on a host OS not
in the stamp, `--check` prints the full divergence report, uploads
the measured result TOML as an artifact, and exits 0 (advisory).
Bench runs `--allow-missing-baseline` (existing behavior). The
first follow-up commit after the wave transplants the CI-measured
artifacts into `status_windows`/`reason_windows` rows + a
`bench-windows-x86_64.json` baseline and adds `"windows"` to the
stamps, flipping all four lanes to blocking — the exact
mechanism-then-measurement two-step RFC 0062 used for Linux bench,
now formalized in the expectations format instead of a CI flag.

Rows known-divergent by construction get seeded
`status_windows`/`reason_windows` entries in this wave (measured
skips, not guesses): the POSIX-only files CPython itself skips on
Windows (`test_fcntl`, `test_posix`, `test_pty`, `test_grp`/
`test_pwd`, `test_ioctl`, …) carry `status_windows = "skip"` with
CPython's own skip reason.

### Non-goals

- **C extensions on Windows.** Loading `.pyd`s built for CPython
  requires a `python313.dll` for the PE import to resolve against —
  WeavePy is a static executable, and the honest fix (restructuring
  the workspace so a `python313.dll` cdylib exports the C-API and
  the exe links it) is its own wave. `EXT_SUFFIX` stays truthful,
  `ExtensionFileLoader` reports a clear error, ecosystem rows
  needing binary wheels get measured `status_windows = "fail"` rows.
  This is the wave after this one, and the RFC-0062 header work is
  its prerequisite on the build side.
- **Console Unicode fidelity (`_WindowsConsoleIO`).** Piped and
  redirected streams — everything CI and tooling see — go through
  the regular fd path landed here. Interactive-console
  `ReadConsoleW`/`WriteConsoleW` IO is deferred; `sys.stdout` on a
  console is UTF-8 CRT IO meanwhile.
- **`_wmi`, `winsound`, `msilib`**, the `py.exe` launcher, MSI/Store
  installers, `WindowsRegistryFinder` (deprecated in CPython, never
  default-active), and aarch64-windows ctypes calls.
- **Measured-blocking Windows lanes in this same commit** — the
  lanes land advisory with the stamp mechanism; flipping requires
  CI-measured artifacts by definition (see WS7).

### Acceptance criteria

1. **Windows compiles and self-tests green, blocking**: `cargo build
   --target x86_64-pc-windows-msvc -p weavepy-vm -p weavepy-cli`
   clean; the `windows-latest` `test` job — including every new
   Windows-gated integration test in WS7's list — passes and gates
   the PR.
2. **No POSIX regression**: regrtest `unexpected 0`, ecosystem 31/31
   + selftests, bench gate, and `weavepy-dist check` all green on
   macOS (and ubuntu in CI), unchanged baselines.
3. **The quartet is real**: `_winapi`, `msvcrt`, `winreg`,
   `_overlapped` register on Windows with the surfaces enumerated
   above; `import asyncio`, `import multiprocessing`, `import
   subprocess`, `import shutil`, `import ctypes` succeed on Windows
   with their Windows arms active (proven by the WS7 tests).
4. **Truthful signals**: `import fcntl`/`termios`/`resource`/`pwd`/
   `grp` raise `ModuleNotFoundError` on Windows;
   `sys.builtin_module_names` reflects the real registration table
   on every OS; `OSError.winerror` is populated from real Win32
   failures.
5. **The artifact exists**: `weavepy-dist build --format zip` on
   Windows produces the NT layout; `weavepy-dist check` passes its
   matrix (minus the cext SKIP) on the `windows-latest` dist-check
   lane.
6. **The measurement machinery lands**: `measured_os` stamps parse
   (with tests), the four Windows CI lanes run and upload measured
   artifacts, and known-POSIX-only rows carry seeded
   `status_windows` skips.
7. **All gates green on macOS**: `cargo fmt`, `clippy -D warnings`,
   `cargo test --workspace`, `regrtest --check`,
   `ecosystem --check`, `weavepy-dist check`.

## Drawbacks

- **A second syscall dialect forever.** Every future fd-touching
  feature now has an NT arm to keep honest; the `windows-sys`
  surface is `unsafe` FFI in exactly the layer RFC goals want
  `unsafe` confined. Mitigation: the CRT/Win32 calls are localized
  in the new modules + the WS1 bridge, mirroring how `libc::` is
  already localized.
- **Cross-compiled confidence has limits.** The wave is developed on
  macOS; `cargo build --target windows-msvc` proves compilation, and
  the CI tests prove behavior — but iteration on a Windows-only
  failure is a CI round-trip. The WS7 test batteries are deliberately
  fine-grained so failures localize.
- **Advisory lanes can rot if the follow-up stalls.** An advisory
  regrtest lane nobody baselines is RFC 0062's "unfalsifiable"
  problem in new clothes. Mitigation: the flip-to-blocking follow-up
  is named in the acceptance story, and the artifact upload makes the
  baseline a copy-paste, not a project.
- **The Win64 asm call gate is high-risk code** reviewed without
  local execution. Mitigation: it is the same shape as the existing
  SysV gates, the ABI is simpler (four register slots), and the CI
  test calls through it with argument patterns covering int/float/
  aggregate/callback cases.

## Alternatives

- **HANDLE-native fd model** (return HANDLEs from `fileno()`,
  skip the CRT): rejected — every consumer contract breaks
  (`msvcrt.get_osfhandle(sys.stdout.fileno())` is real-world code;
  `os.close` on a HANDLE double-frees when CRT-backed files exist),
  and CPython's model costs one `_open_osfhandle` per open.
- **Adopt CPython's `os.py` + a native `nt` module** (invert the
  ownership to CPython's architecture): rejected for this wave —
  WeavePy's Rust-`os`-owns model is load-bearing for every platform
  (the frozen `posix`/`nt` shims re-export it), and flipping the
  ownership is a cross-platform refactor with no user-visible gain;
  the shim approach reaches the same importable surface.
- **SelectorEventLoop as the Windows asyncio default** (skip
  `_overlapped`): rejected — CPython 3.13's default is Proactor;
  libraries probe the loop class and subprocess support requires it;
  shipping the non-default loop is a behavior divergence exactly
  where "drop-in" claims live.
- **libffi for the Win64 gate** instead of extending the hand-rolled
  one: rejected — the project already owns SysV gates and a second
  FFI backend for one ABI adds a C build dependency the workspace
  deliberately avoids.
- **`Lib\` at the artifact root (full CPython layout)** instead of
  keeping `lib/weavepy3.13`: rejected — the landmark walk, the
  materializer, venv resolution, and the POSIX artifact all share
  one layout constant; a per-OS stdlib directory name buys cosmetic
  similarity and costs a second identity to test. The exe-at-root
  and `Scripts\` conventions are kept because *code* (venv, pip,
  sysconfig schemes) observes them.
- **Waiting for the python313.dll wave and doing C extensions
  simultaneously**: rejected — the pure-Python drop-in story
  (stdlib + pip + venv + asyncio + multiprocessing) is
  independently shippable and independently measurable, and the DLL
  restructure is lower-risk once the runtime beneath it is proven.

## Prior art

- **CPython's own NT port** (`Modules/posixmodule.c`, `PC/`): the
  CRT-fd model, errmap, and console-ctrl-handler design are
  transcribed, not reinvented.
- **PyPy on Windows**: reimplemented the same quartet
  (`_winapi` in RPython) and documented that proactor-asyncio and
  multiprocessing were unusable until `_overlapped` landed —
  evidence for including it in the first wave.
- **python-build-standalone**: ships CPython for Windows with
  venv-copies-the-exe (no launcher), validating WS6's venv approach.
- **Rust ecosystem**: `windows-sys` is the Microsoft-maintained
  binding crate used by std itself; `socket2` (already a dependency)
  is Windows-clean, which is why sockets are the most portable
  module today.
- **RustPython**: has a partial `_winapi`/`winreg`; its issue
  tracker's recurring Windows-fd bugs (HANDLE/fd confusion in mmap
  and subprocess) are the cautionary tale WS1's single-owner CRT
  rule is designed against.

## Unresolved questions

- Whether `os.pipe` fds should default non-inheritable via
  `O_NOINHERIT` at `_pipe` time or via `SetHandleInformation` after
  (CPython does the latter; behaviorally identical, decided at
  implementation).
- Whether the `measured_os` stamp belongs in `expectations.toml` or
  a sibling file — this RFC puts it in the header of the same file
  (one source of truth), matching the `timeout_seconds` precedent.
- How much of `test_winreg`/`test_winapi`'s surface the first
  measured Windows baseline can claim — answered by the first CI
  sweep, by design.

## Future work

- **The `python313.dll` wave**: restructure so the C-API exports
  live in a cdylib the exe links; binary wheels (numpy et al.) and
  `pip install` of C sdists via MSVC vars then follow — the Windows
  twin of RFC 0062's WS2.
- `_WindowsConsoleIO` for interactive-console Unicode fidelity.
- The flip-to-blocking baseline commit (measured
  `status_windows` rows, `bench-windows-x86_64.json`,
  `measured_os += ["windows"]`) — first follow-up after this wave.
- aarch64-windows ctypes call gate; ARM64 runner lanes when GitHub
  offers them.
- `py.exe`-style launcher behaviors, Start-menu/installer UX,
  code signing.
- `os.add_dll_directory` + DLL search-path hardening once the DLL
  wave exists.

## Results

*(To be filled in at landing, per repo convention: measured CI
outcomes for the Windows test battery, the advisory-lane first
sweeps, and the unchanged macOS/Linux baselines.)*
