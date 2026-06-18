# RFC 0040: Systems wave — faithful process model (subprocess / multiprocessing / signal) and the I/O, filesystem, and archive tail

- **Status**: Accepted
- **Authors**: WeavePy authors
- **Created**: 2026-06-17
- **Tracking issue**: TBD
- **Builds on**: RFC 0024 (real OS threads, GIL, cycle GC), RFC 0025
  (cross-thread `Arc`-rooted heap), RFC 0026 (the first `multiprocessing`
  pass), RFC 0031 (observability hot path — the eval breaker fires
  audit/signal hooks), RFC 0036/0037/0038/0039 (the measured `Lib/test/`
  sweep waves 1–4). RFC 0039's "Future work" names this wave directly
  (`multiprocessing` completion, the deferred `subprocess`/pidfd story).

## Summary

Wave 4 (RFC 0039) closed the in-process concurrency story: real OS
threads under a faithful GIL, a generational cycle collector, blocking
`queue`, a `ThreadPoolExecutor`, native selectors, and a real
`SelectorEventLoop`. It left the baseline at roughly **70 `pass` /
49 `fail` / 25 `skip` / 1 `timeout`** against the vendored CPython 3.13
suite.

The single largest *category* of real-world Python still unmet is the
**systems layer**: spawning and talking to other processes, signals, and
high-fidelity file/stream I/O. Concretely:

1. **`subprocess` is entirely skipped.** Today `subprocess.py` is a
   363-line shim over a Rust core that just runs `std::process::Command`
   and shells out to `kill(1)` for signals — there is no real `Popen`,
   no file-descriptor plumbing, no `communicate()` deadlock-avoidance,
   no `preexec_fn`/`pass_fds`/`start_new_session`. Every build tool,
   test runner, language server, and CLI wrapper that shells out is
   therefore unsupported.
2. **`multiprocessing` works only for the bundled fixtures.** The
   CPython `test_multiprocessing_{fork,spawn,forkserver,main_handling}`
   files are skipped: the module is not a faithful package
   (`connection`, `pool`, `managers`, `synchronize`, `reduction`,
   `resource_tracker`, `sharedctypes`) and lacks the `_multiprocessing`
   primitives the tests probe.
3. **`signal` is skipped.** RFC 0039 shipped the OS-signal subsystem
   (`test_threadsignals` passes), but `test_signal` needs the full
   `signal` module surface: `signal()`/`getsignal`, `sigwait`/
   `sigwaitinfo`/`sigtimedwait`, `pthread_kill`/`pthread_sigmask`,
   `setitimer`/`getitimer`, `set_wakeup_fd`, `siginterrupt`, and the
   `Signals`/`Handlers`/`Sigmasks` enums.
4. **The `io` module diverges in its tail.** `test_io` fails on
   `BufferedRandom` read/write interleaving, `TextIOWrapper` seek/tell
   cookie semantics, `BufferedIOBase.readinto` over a custom raw stream,
   and `io.open(fd:int)`.
5. **Filesystem breadth and archives lag.** `os` is missing
   `environb`/`device_encoding`/`closerange`/`fork`/`exec*`/`posix_spawn`/
   `wait`/`W*` macros/`setsid`/`killpg`/`register_at_fork`; `tarfile`
   lacks PAX/GNU long-name headers and the `r|`/`w|` stream modes;
   `zipfile` lacks the `Path` accessor and per-file compression options;
   `shutil.make_archive` can't write a directory tree into a zip; and
   `tempfile.SpooledTemporaryFile` lacks a faithful I/O layer.

This wave makes all five faithful. It is the systems-programming
counterpart to RFC 0039's concurrency work, and it is the prerequisite
for the next two roadmap items (live-network grading and a real
`ProcessPoolExecutor`).

As with every wave, the deliverable is **measured, not aspirational**:
each workstream names the `expectations.toml` rows it flips, lands at
least one bundled in-process fixture, and the commit is not done until a
fresh subprocess sweep is `--check` clean with the touched rows rewritten
to their measured status.

## Motivation

The README's promise is "run existing Python code, packages, tools, and
workflows unchanged." The two most common things real tools do that
WeavePy can't yet are (a) **shell out** (`subprocess.run`,
`Popen.communicate`) and (b) **fork worker pools**
(`multiprocessing.Pool`, `ProcessPoolExecutor`). Both are *capability*
gaps, not fidelity nits — the relevant tests are skipped, not failing.

Two things make this the right next wave:

1. **The hard parts already exist.** RFC 0025 made the heap `Send +
   Sync`; RFC 0039 shipped real threads, the GIL hand-off, native
   selectors, and the OS-signal trampoline. Spawning a child and wiring
   pipes is now "finish the primitives + port CPython's pure-Python
   driver," not a from-scratch runtime change.
2. **Fork/exec is already exercised here.** The conformance harness runs
   each test in a spawned `weavepy` child (`--mode subprocess`), and the
   bundled `multiprocessing` fixtures spawn and reap children, so the
   sandbox permits `fork`/`exec`/`waitpid`. That de-risks the central
   primitive.

## CPython reference

This RFC matches **CPython 3.13** as defined by:

- **subprocess** — `Lib/subprocess.py` (the `Popen` driver,
  `_execute_child`, `communicate`, `_USE_POSIX_SPAWN`),
  `Modules/_posixsubprocess.c` (`fork_exec`: the async-signal-safe child
  that dups the pipe fds, closes inherited fds, restores signals,
  `setsid`/`setpgid`, `chdir(cwd)`, then `execv(p)e`, reporting failure
  through the error pipe as `b"exc:hexerrno:msg"`).
- **multiprocessing** — `Lib/multiprocessing/` (`context`, `process`,
  `connection`, `queues`, `pool`, `managers`, `synchronize`,
  `reduction`, `resource_tracker`, `popen_fork`/`popen_spawn_posix`/
  `popen_forkserver`, `spawn`, `util`), `Modules/_multiprocessing/`
  (`SemLock`).
- **signal** — `Modules/signalmodule.c` (`signal`/`getsignal`,
  `default_int_handler`, `sigwait`/`sigwaitinfo`/`sigtimedwait`,
  `pthread_kill`/`pthread_sigmask`, `setitimer`/`getitimer`,
  `set_wakeup_fd`, `siginterrupt`, `strsignal`, the `Signals`/`Handlers`/
  `Sigmasks` `IntEnum`s).
- **io** — `Modules/_io/` (`BufferedReader`/`BufferedWriter`/
  `BufferedRandom` shared-buffer pointer arithmetic, `bufferedio.c`
  read/write switching with `_bufferedreader_reset_buf`;
  `TextIOWrapper` the snapshot/cookie `tell()` in `textio.c`;
  `FileIO` over an integer fd), `Lib/_pyio.py` as the behavioural spec.
- **tarfile / zipfile / shutil** — `Lib/tarfile.py` (PAX/GNU headers,
  stream modes), `Lib/zipfile/` (incl. `zipfile.Path` /
  `importlib.resources` glue), `Lib/shutil.py`
  (`make_archive`/`_make_zipfile`).
- **tempfile** — `Lib/tempfile.py` (`SpooledTemporaryFile` rollover and
  the `_io`-delegating wrapper).

PEP 703 free-threading and per-interpreter GIL remain out of scope (the
GIL stays, matching CPython 3.13). Windows process creation
(`CreateProcess`) is out of scope; this wave targets the POSIX path and
keeps the non-POSIX arms as `NotImplementedError`, matching how the
existing `os` primitives are gated.

## Current baseline (measured starting point)

- `cargo build --workspace` is green.
- Bundled `tests/regrtest/` suite is `--check` clean (`unexpected 0`).
- CPython `Lib/test/` allowlist: ~70 `pass`, ~49 `fail`, ~25 `skip`,
  1 `timeout`.

The systems-cluster rows this wave targets, with their *committed*
status and root cause:

| Row | Status | Root cause |
|---|---|---|
| `test_subprocess` | skip | no real `Popen` (`fork_exec`) |
| `test_multiprocessing_fork` | skip | no faithful `multiprocessing` package |
| `test_multiprocessing_spawn` | skip | ″ |
| `test_multiprocessing_forkserver` | skip | ″ |
| `test_multiprocessing_main_handling` | skip | ″ |
| `test_signal` | skip | partial `signal` surface |
| `test_concurrent_futures` | skip | ProcessPool needs mp |
| `test_io` | fail | BufferedRandom / TextIOWrapper cookie / readinto / open(fd) |
| `test_os` | fail | `environb`/`device_encoding`/scandir/process breadth |
| `test_posix` | fail | `posix_spawn` / `os.scheduler_*` |
| `test_posixpath` | fail | (WTF-8; out of scope — see Non-goals) |
| `test_tarfile` | fail | PAX/GNU headers + stream modes |
| `test_zipfile` | fail | `zipfile.Path` + per-file compression |
| `test_shutil` | fail | `make_archive` zip directory handling |
| `test_tempfile` | fail | `SpooledTemporaryFile` I/O layer |

## Detailed design

Nine workstreams (WS1–WS9), sequenced in dependency order. Each lists
the affected crate(s)/module(s) and the rows it is expected to flip.
Line-count estimates include Rust glue, verbatim/ported frozen Python,
and tests.

### WS1 — `os` process & fd primitives (`weavepy-vm/src/stdlib/os.rs`) · ~3K LOC

The foundation everything else rides on. Add, behind `#[cfg(unix)]`:
`fork`, `_exit`, `execv`/`execve`/`execvp`/`execvpe`/`_execvpe`,
`posix_spawn`/`posix_spawnp` (via `libc::posix_spawn` + a
`posix_spawn_file_actions`/`posix_spawnattr` builder), `wait`/`wait3`/
`wait4`, the `WIFEXITED`/`WEXITSTATUS`/`WIFSIGNALED`/`WTERMSIG`/
`WIFSTOPPED`/`WSTOPSIG`/`WIFCONTINUED`/`WCOREDUMP` status macros, the
`WNOHANG`/`WUNTRACED`/… option constants, `closerange`, `pipe2`,
`setsid`/`getsid`/`setpgid`/`getpgid`/`getpgrp`/`tcgetpgrp`/`tcsetpgrp`,
`killpg`, `openpty`/`forkpty`/`login_tty`, `register_at_fork`,
`sched_getaffinity`/`sched_setaffinity`, `cpu_count` already present;
`environb` (a bytes view over `environ`), `device_encoding`, and the
`scandir`/`stat_result` edges `test_os` probes. `get_exec_path` and
`fsencode`/`fsdecode` already exist; verify they satisfy
`subprocess`/`_execvpe`.

**Flips:** prerequisite for WS3/WS5; directly moves `test_os`/`test_posix`.

### WS2 — `_posixsubprocess.fork_exec` (`weavepy-vm`, new `posixsubprocess_mod.rs`) · ~2K LOC

A native module exposing the CPython-3.13 `fork_exec(...)` 24-argument
primitive. The parent builds the C-string `argv`/`envp` and the
fds-to-keep list *before* forking (all allocation done up front). The
child, using only async-signal-safe libc calls: optionally runs
`preexec_fn` (via `interp.call_object`, best-effort, matching CPython's
documented "unsafe" contract), restores signal dispositions
(`SIG_DFL` for the restore set), `setsid()`/`setpgid()` per
`start_new_session`/`process_group`, applies `gid`/`uid`/`umask` when
requested, `chdir(cwd)`, dups `p2c`/`c2p`/`err` onto 0/1/2, closes all
other fds except `fds_to_keep` (using `closerange` around the kept set),
then walks `executable_list` calling `execve`/`execv`. On any failure it
writes `b"<ExcName>:<hexerrno>:<msg>"` to `errpipe_write` and `_exit`s.
A `os.posix_spawn` path (WS1) covers the `_USE_POSIX_SPAWN` fast lane.

**Flips:** the structural prerequisite for `test_subprocess`.

### WS3 — Faithful `subprocess` (frozen Python, verbatim port) · ~2K LOC

Replace the 363-line shim with CPython's `Lib/subprocess.py` ported
verbatim (POSIX arm): `Popen.__init__`, `_get_handles` (pipe creation),
`_execute_child` over the WS2 `fork_exec` and the WS1 `posix_spawn`,
`communicate` with the selector-backed `_communicate` (no deadlock on
large output), `_handle_exitstatus`, `wait(timeout)`, `poll`,
`send_signal`/`terminate`/`kill`, `run`/`call`/`check_call`/
`check_output`/`getstatusoutput`/`getoutput`, `CalledProcessError`/
`TimeoutExpired`/`CompletedProcess`, `DEVNULL`/`PIPE`/`STDOUT`, and the
`list2cmdline`/`_args_from_interpreter_flags` helpers. The Rust
`_subprocess` shim is retired; `_posixsubprocess` is the only native
dependency.

**Flips:** `test_subprocess`.

### WS4 — Full `signal` surface (`weavepy-vm/src/stdlib/signal_mod.rs`) · ~2K LOC

Grow the existing OS-signal subsystem to the full CPython surface:
`signal()`/`getsignal`/`default_int_handler`/`strsignal`/`valid_signals`,
`sigwait`/`sigwaitinfo`/`sigtimedwait`, `pthread_kill`/`pthread_sigmask`,
`setitimer`/`getitimer` (+ `ITIMER_*`), `set_wakeup_fd`, `siginterrupt`,
`raise_signal`/`alarm`/`pause` (present — verify), and the `Signals`/
`Handlers`/`Sigmasks` `IntEnum`s built over the frozen `enum`. The
trampoline already trips a flag + writes the wakeup fd (RFC 0039);
`set_wakeup_fd` makes the fd user-settable and `pthread_sigmask` lets the
threading tests block/unblock on worker threads.

**Flips:** `test_signal`.

### WS5 — Faithful `multiprocessing` package (frozen Python + `_multiprocessing` core) · ~9K LOC

Port CPython's `multiprocessing/` as a real package over a thin
`_multiprocessing` Rust core (`SemLock` via POSIX named semaphores,
`sem_unlink`, `recvfd`/`sendfd` over `SCM_RIGHTS`, `flock`). Modules:
`context` (the `fork`/`spawn`/`forkserver` start methods),
`process.BaseProcess`, `connection` (`Pipe`, `Listener`/`Client`,
socket + named-pipe transports, the `wait()` selector multiplexer),
`queues` (`Queue`/`SimpleQueue`/`JoinableQueue` over `Pipe` + a feeder
thread), `pool.Pool`, `managers` (the server process + proxies),
`synchronize` (`Lock`/`RLock`/`Semaphore`/`Event`/`Condition`/`Barrier`
over `SemLock`), `reduction` (ForkingPickler + fd passing),
`resource_tracker`, `sharedctypes`/`shared_memory`, `heap`, `util`. The
`spawn` child re-execs `weavepy --multiprocessing-* <fd>` (the existing
`_spawn_child` helper, generalised); `fork` clones the interpreter via
the RFC 0025 shared heap.

**Flips:** `test_multiprocessing_{fork,spawn,forkserver,main_handling}`.

### WS6 — `ProcessPoolExecutor` (frozen `concurrent.futures.process`) · ~1.5K LOC

The frozen `concurrent_futures_process.py` is already present; wire it to
the WS5 `multiprocessing` context (`SpawnContext`/`ForkContext`), the
`_ExecutorManagerThread`, and `BrokenProcessPool` handling. With WS5 the
package import no longer blocks, so the harness can grade
`test_concurrent_futures` as a unit.

**Flips:** `test_concurrent_futures`.

### WS7 — `io` fidelity (`weavepy-vm/src/stdlib/io.rs`, `io_full.rs`) · ~4K LOC

Close the measured `test_io` gaps: `BufferedRandom` (a single shared
buffer with CPython's read/write-mode switching and `seek` reset),
`BufferedReader.readinto`/`readinto1` over an arbitrary raw stream that
only implements `readinto`, `TextIOWrapper.tell()`/`seek()` opaque-cookie
semantics (decoder-state snapshot), `io.open(fd:int)` / `FileIO(fd)`
constructing over a borrowed descriptor, and the
`detach()`/`.newlines`/`truncate()` surface `test_tempfile`/`test_tarfile`
also need.

**Flips:** `test_io`; unblocks `test_tempfile`/`test_tarfile` tails.

### WS8 — Archives + `tempfile` (frozen Python) · ~4K LOC

- `tarfile`: PAX (`pax_headers`) and GNU long-name/long-link extended
  headers, `r|`/`r|gz`/`w|` stream modes over `_Stream`, `ExFileObject`
  `.name`, and the `truncate()` the buffered file now supports (WS7).
- `zipfile`: the `zipfile.Path`/`CompleteDirs` accessor, per-file
  `compresslevel`, `BZIP2`/`LZMA` members, and directory entries so
  `ZipFile.write(dir)` and `shutil.make_archive(..., "zip")` work.
- `tempfile.SpooledTemporaryFile`: a faithful rollover wrapper that
  delegates to the WS7 `io` objects (`detach`, `truncate`, `.newlines`).

**Flips:** `test_tarfile`, `test_zipfile`, `test_shutil`, `test_tempfile`.

### WS9 — Fixtures + measured baseline rewrite · ~1.5K LOC

One bundled in-process fixture per workstream under `tests/regrtest/`
(`sysproc_subprocess_pipe.py`, `sysproc_signal_roundtrip.py`,
`sysproc_mp_pool.py`, `sysproc_process_pool.py`, `io_buffered_random.py`,
`archive_tar_zip_roundtrip.py`), plus the `test.support` process helpers
the cluster imports (`script_helper`, `os_helper` gaps). Rewrite every
touched `expectations.toml` row to its **measured** status; commit
complete only when `--check` reports `unexpected 0`.

## Measured targets

Wave 5's acceptance bar is flipping these rows to `pass`:

| Cluster | Target rows (→ `pass`) |
|---|---|
| WS2/WS3 subprocess | `test_subprocess` |
| WS4 signal | `test_signal` |
| WS5 multiprocessing | `test_multiprocessing_{fork,spawn,forkserver,main_handling}` |
| WS6 futures | `test_concurrent_futures` |
| WS7 io | `test_io` |
| WS8 archives | `test_tarfile`, `test_zipfile`, `test_shutil`, `test_tempfile` |
| WS1 os | `test_os`, `test_posix` (advance; measured-rewrite the tail) |

That is **~12 rows flipping `skip`/`fail` → `pass`**. Rows that prove
deeper than estimated are rewritten to a measured `reason` and deferred
rather than expanding the commit.

## Non-goals / Drawbacks

- **WTF-8 string storage stays out of scope.** `test_posixpath`'s sole
  remaining failure (lone-surrogate `realpath`) and `test_codecs` share
  a str-representation root cause tracked in its own arc; this wave does
  not touch str storage.
- **Windows process creation is deferred.** The POSIX path is faithful;
  the Windows arms raise `NotImplementedError`, matching the existing
  `os` primitives.
- **`fork()` in a threaded runtime is inherently delicate.** The child
  between `fork` and `exec` runs only async-signal-safe code (WS2);
  `multiprocessing`'s `fork` start method clones the shared `Arc` heap
  and is documented (as in CPython) as unsafe with live threads. `spawn`
  is the default and the safe path.
- **`preexec_fn` runs Python in the forked child.** This is unsafe by
  construction (CPython documents the same); supported best-effort for
  the common cases the tests exercise.
- **Breadth.** Nine workstreams across the VM `os` core, a new native
  module, the `signal` subsystem, and several large frozen ports is a lot
  of surface. Capped by the per-workstream fixtures and the `--check`
  gate.

## Alternatives

- **`posix_spawn`-only (no `fork_exec`).** Simpler and avoids the
  async-signal-safe child, but `posix_spawn` can't express `preexec_fn`,
  `pass_fds` beyond 0/1/2, `close_fds`, or `start_new_session` portably —
  exactly the surface `test_subprocess` asserts. Rejected; we ship both
  and pick `posix_spawn` only on CPython's `_USE_POSIX_SPAWN` fast path.
- **Keep the `std::process::Command` shim and document the divergence.**
  Lowest effort, but it's the capability gap this wave exists to close
  (no fd plumbing, no `communicate`). Rejected.
- **A pure-Python `multiprocessing` over threads only.** Already exists
  as the `set_start_method("thread")` fallback; it isn't real
  parallelism and fails the `test_multiprocessing_*` semantics. Kept as
  a fallback, not the default.
- **Port CPython's `_io` C module verbatim.** Tempting for `io`, but
  WeavePy already has a substantial native `io`; we close the measured
  behavioural gaps rather than rewrite the layer.

## Prior art

- **CPython 3.13** — every decision tracks it: the `_posixsubprocess`
  child sequence, the `_USE_POSIX_SPAWN` heuristic, the `multiprocessing`
  start-method split, the `signal` wakeup-fd model, and the buffered/text
  I/O pointer arithmetic.
- **PyPy** — ships CPython's `subprocess`/`multiprocessing` essentially
  verbatim over its own `_posixsubprocess`/`_multiprocessing`,
  confirming the "port the driver, implement the primitive" split.
- **RFC 0026 / 0039** — the in-tree foundation; this wave finishes the
  `multiprocessing` pass RFC 0026 started and the process/signal tail
  RFC 0039 deferred.

## Future work

- **Live-network grading**: a fixture HTTP/echo-server harness so
  `test_socket`/`test_httplib`/`test_asyncio`'s networked subset can be
  graded (RFC 0039 future work; now unblocked by real `subprocess`).
- **Windows process model**: `CreateProcess`, named-pipe connections,
  the `spawn_main` Windows entry.
- **WTF-8 str + codecs** (its own wave): `surrogateescape`/
  `surrogatepass`, `test_codecs`, `test_posixpath`'s last failure.
- **C-accelerator parity**: `_decimal`, the `_datetime` py/C split,
  `_json`, a faithful `_csv`, `_statistics`, `math.fma`.
