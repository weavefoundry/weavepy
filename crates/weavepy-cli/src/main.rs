//! The `weavepy` command-line interpreter.
//!
//! Argv-compatible with `python(1)` 3.13: every flag in the CPython
//! manpage is parsed and honoured (those we can't yet act on are
//! accepted and forwarded onto `sys.flags` / `sys._xoptions` so user
//! code that introspects them sees realistic values). Modes:
//!
//! ```text
//! weavepy [flags] [-c command | -m module | script | -] [args ...]
//! weavepy [flags]                                     -- interactive REPL
//! ```
//!
//! Environment variables (`PYTHON*`) are read after the flag table is
//! parsed and folded in unless `-E` / `-I` says otherwise.

mod regrtest_cmd;
mod repl;

use std::{
    env, fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::ExitCode,
};

use anyhow::{Context, Result};
use clap::{ArgAction, Parser};
use tracing_subscriber::EnvFilter;

use weavepy::{InterpreterFlags, RunOptions};

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Recognised subcommands. We thread them through manually instead of
/// using `clap`'s `#[command(subcommand)]` because the bare `weavepy`
/// CLI already overloads the positional `script` slot. Detecting these
/// up front in `main()` keeps the unsugar trivial.
const SUBCOMMANDS: &[&str] = &["regrtest"];

/// Run a `weavepy --multiprocessing-fork <kwds...>` child. The vendored
/// `multiprocessing.popen_spawn_posix`/`popen_forkserver` re-exec us with
/// CPython's frozen command line: `argv == [exe, "--multiprocessing-fork",
/// "tracker_fd=N", "pipe_handle=M", …]`. We must therefore preserve the real
/// argv (so `spawn.is_forking(sys.argv)` holds and the `name=value` kwds are
/// parseable) and hand off to `multiprocessing._run_spawn_child()`, which
/// mirrors CPython's `spawn.spawn_main` POSIX body and *returns* the child's
/// exit code (rather than `sys.exit`-ing, so the Rust bridge controls the
/// process status).
fn run_multiprocessing_child(raw: &[String]) -> ExitCode {
    // `_run_spawn_child` runs the worker target via `spawn._main` and returns
    // its exit code; `_multiprocessing._exit(code)` then `std::process::exit`s
    // directly, so the `Ok(())` arm is only reached on a clean fall-through.
    // CPython's `spawn_main` ends in `sys.exit(exitcode)`, whose interpreter
    // finalization runs `atexit` handlers (the worker may register its own,
    // e.g. gh-83856 / `test_atexit`, plus `multiprocessing.util._exit_function`).
    // Our `_multiprocessing._exit` is a hard `std::process::exit` that bypasses
    // the CLI's normal shutdown drain, so run the exit funcs explicitly first.
    let snippet = "import multiprocessing, _multiprocessing, atexit as _atexit\n\
                   _mp_code = multiprocessing._run_spawn_child()\n\
                   _atexit._run_exitfuncs()\n\
                   _multiprocessing._exit(int(_mp_code) if _mp_code is not None else 0)\n";
    // The parent's `spawn.get_command_line()` emits
    // `[exe, <interp opts...>, "--multiprocessing-fork", "name=value", ...]`,
    // mirroring CPython so the child inherits `-O`/`-S`/`-E`/`-I`/`-X dev`/…
    // (`test_multiprocessing.TestFlags.test_flags`). Split at the
    // `--multiprocessing-fork` marker: everything before it is interpreter
    // flags we must apply to the child; the marker plus the `name=value` kwds
    // become `sys.argv[1:]` so `spawn.is_forking(sys.argv)` still holds.
    let exe = raw.first().cloned().unwrap_or_else(|| "weavepy".to_owned());
    let fork_idx = raw
        .iter()
        .position(|a| a == "--multiprocessing-fork")
        .unwrap_or(usize::from(!raw.is_empty()));
    let opt_args = if fork_idx > 1 {
        &raw[1..fork_idx]
    } else {
        &[][..]
    };
    let tail = if fork_idx < raw.len() {
        &raw[fork_idx..]
    } else {
        &[][..]
    };
    let flags = child_flags_from_opts(&exe, opt_args);
    let mut argv = vec![exe];
    argv.extend(tail.iter().cloned());
    let opts = RunOptions::new("<multiprocessing-fork>")
        .with_argv(argv)
        .with_flags(flags);
    match weavepy::run_source_with_options(snippet, &opts) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            let mut stderr = io::stderr().lock();
            let _ = writeln!(stderr, "{}", err.format(snippet, "<multiprocessing-fork>"));
            ExitCode::from(1)
        }
    }
}

/// Build the child interpreter flags for a `--multiprocessing-fork` re-exec by
/// re-parsing the interpreter-flag opts the parent placed before the marker
/// (`-O`/`-S`/`-E`/`-I`/`-X dev`/…) through the same clap table + env overrides
/// the normal launch path uses. Falls back to defaults if the opts don't parse
/// (they always should — they come from `_args_from_interpreter_flags()`).
fn child_flags_from_opts(exe: &str, opt_args: &[String]) -> InterpreterFlags {
    let parse_argv: Vec<String> = std::iter::once(exe.to_owned())
        .chain(opt_args.iter().cloned())
        .collect();
    match Cli::try_parse_from(&parse_argv) {
        Ok(cli) => {
            let env = if cli.isolated || cli.ignore_env {
                EnvOverrides::ignored()
            } else {
                EnvOverrides::from_env()
            };
            build_flags(&cli, &env)
        }
        Err(_) => InterpreterFlags::default(),
    }
}

/// CPython 3.13's `python(1)` flag set.
///
/// Defaults match invoking `python` with no flags. Most of the
/// surface is "accept and propagate" — `sys.flags`, `sys._xoptions`,
/// `sys.warnoptions` reflect the user's choice even when the flag's
/// behaviour is partial.
#[derive(Debug, Parser, Clone, Default)]
#[command(
    name = "weavepy",
    bin_name = "weavepy",
    version = VERSION,
    about = "WeavePy: a high-performance, CPython-compatible Python interpreter written in Rust.",
    disable_version_flag = true,
    disable_help_flag = true,
    trailing_var_arg = true,
    allow_hyphen_values = true,
)]
struct Cli {
    /// Print the version and exit (`python -V` / `--version`).
    #[arg(short = 'V', long = "version", action = ArgAction::SetTrue, overrides_with = "version")]
    version: bool,

    /// Print this help and exit.
    #[arg(short = 'h', long = "help", action = ArgAction::SetTrue, overrides_with = "help")]
    help: bool,

    /// Print the help-env summary (which `PYTHON*` vars are honoured) and exit.
    #[arg(long = "help-env", action = ArgAction::SetTrue, overrides_with = "help_env")]
    help_env: bool,

    /// Print the help-xoptions summary and exit.
    #[arg(long = "help-xoptions", action = ArgAction::SetTrue, overrides_with = "help_xoptions")]
    help_xoptions: bool,

    /// Optimisation level. `-O` once, `-OO` twice.
    #[arg(short = 'O', action = ArgAction::Count)]
    optimize: u8,

    /// `bytes`/`str` comparison warnings. `-b` once warns, `-bb` errors.
    #[arg(short = 'b', action = ArgAction::Count)]
    bytes_warning: u8,

    /// Don't write `.pyc` files.
    #[arg(short = 'B', action = ArgAction::SetTrue, overrides_with = "no_bytecode_write")]
    no_bytecode_write: bool,

    /// Parser debug output (`sys.flags.debug`; counted like CPython's
    /// `-d`, otherwise a no-op stub).
    #[arg(short = 'd', action = ArgAction::Count)]
    parser_debug: u8,

    /// `-R`: turn on hash randomization (the default; overrides a
    /// `PYTHONHASHSEED` fixed seed, like CPython).
    #[arg(short = 'R', action = ArgAction::SetTrue, overrides_with = "hash_randomization")]
    hash_randomization: bool,

    /// Ignore all `PYTHON*` environment variables.
    #[arg(short = 'E', action = ArgAction::SetTrue, overrides_with = "ignore_env")]
    ignore_env: bool,

    /// Drop into the REPL after running the script / module / command.
    #[arg(short = 'i', action = ArgAction::SetTrue, overrides_with = "inspect_after")]
    inspect_after: bool,

    /// Isolated mode: implies `-E -s` and sets `sys.flags.isolated`.
    #[arg(short = 'I', action = ArgAction::SetTrue, overrides_with = "isolated")]
    isolated: bool,

    /// Don't run `site.main()` on interpreter startup.
    #[arg(short = 'S', action = ArgAction::SetTrue, overrides_with = "no_site")]
    no_site: bool,

    /// Don't add the user site-packages to `sys.path`.
    #[arg(short = 's', action = ArgAction::SetTrue, overrides_with = "no_user_site")]
    no_user_site: bool,

    /// Suppress the REPL banner.
    #[arg(short = 'q', action = ArgAction::SetTrue, overrides_with = "quiet")]
    quiet: bool,

    /// Don't prepend the script dir / cwd to `sys.path`.
    #[arg(short = 'P', action = ArgAction::SetTrue, overrides_with = "safe_path")]
    safe_path: bool,

    /// Force stdout/stderr unbuffered.
    #[arg(short = 'u', action = ArgAction::SetTrue, overrides_with = "unbuffered")]
    unbuffered: bool,

    /// Verbose imports.
    #[arg(short = 'v', action = ArgAction::Count)]
    verbose: u8,

    /// Skip the first source line (shebang trick).
    #[arg(short = 'x', action = ArgAction::SetTrue, overrides_with = "skip_first_line")]
    skip_first_line: bool,

    /// `-X key[=value]`. Forwarded to `sys._xoptions`.
    #[arg(short = 'X', action = ArgAction::Append, value_name = "OPT")]
    xoptions: Vec<String>,

    /// `-W filter` warning control. Forwarded to `sys.warnoptions`.
    #[arg(short = 'W', action = ArgAction::Append, value_name = "FILTER")]
    warnings: Vec<String>,

    /// `--check-hash-based-pycs MODE`. Accepted, ignored (we always
    /// use mtime-mode cache invalidation).
    #[arg(long = "check-hash-based-pycs", value_name = "MODE")]
    check_hash_pycs: Option<String>,

    /// Execute `<command>` as `__main__`. Mirrors `python -c`.
    #[arg(short = 'c', value_name = "SOURCE")]
    command: Option<String>,

    /// Run library module `<MODULE>` as `__main__`. Mirrors `python -m`.
    #[arg(short = 'm', value_name = "MODULE")]
    module: Option<String>,

    /// Script path (`script.py`) or `-` for stdin. Optional.
    script: Option<PathBuf>,

    /// Trailing arguments → `sys.argv[1:]`.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
}

const DIAGNOSTIC_SENTINEL: &str = "exited with diagnostic";

const HELP_BODY: &str = "\
usage: weavepy [option] ... [-c cmd | -m mod | file | -] [arg] ...
Options (and corresponding environment variables):
-b     : issue warnings about converting bytes/bytearray to str (-bb: error)
-B     : don't write .pyc files on import; also PYTHONDONTWRITEBYTECODE=x
-c cmd : program passed in as string (terminates option list)
-d     : turn on parser debugging output (for experts only)
-E     : ignore PYTHON* environment variables (such as PYTHONPATH)
-h     : print this help message and exit (also --help)
-i     : inspect interactively after running script; (also PYTHONINSPECT=x)
-I     : isolate Python from the user's environment (implies -E and -s)
-m mod : run library module as a script (terminates option list)
-O     : remove assert and __debug__-dependent statements; also PYTHONOPTIMIZE=x
-OO    : do -O changes and also discard docstrings
-P     : don't prepend a potentially unsafe path to sys.path
-q     : don't print version and copyright messages on interactive startup
-R     : turn on hash randomization; also PYTHONHASHSEED=random (default)
-s     : don't add user site directory to sys.path; also PYTHONNOUSERSITE
-S     : don't imply 'import site' on initialization
-u     : force the stdout and stderr streams to be unbuffered
-v     : verbose (trace import statements); also PYTHONVERBOSE=x
-V     : print the WeavePy version number and exit (also --version)
-W arg : warning control; arg is action:message:category:module:lineno
-x     : skip first line of source, allowing use of non-Unix shebang
-X opt : set implementation-specific option
file   : program read from script file
-      : program read from stdin (default; interactive mode if a tty)
arg ...: arguments passed to program in sys.argv[1:]
";

const HELP_ENV: &str = "\
Environment variables:
PYTHONHOME            : alternate <prefix> directory (or <prefix>:<exec_prefix>).
                        The default module search path uses <prefix>/python{X.Y}.
PYTHONPATH            : ':'-separated list of directories prefixed to sys.path.
PYTHONSTARTUP         : file executed on interactive startup (no default).
PYTHONOPTIMIZE        : same as -O option.
PYTHONDEBUG           : same as -d option.
PYTHONINSPECT         : same as -i option.
PYTHONUNBUFFERED      : same as -u option.
PYTHONVERBOSE         : same as -v option.
PYTHONNOUSERSITE      : same as -s option.
PYTHONHASHSEED        : if set to 'random', randomize hash; integer in [0, 4294967295] for repeatable.
PYTHONIOENCODING      : Encoding[:errors] used for stdin/stdout/stderr.
PYTHONDONTWRITEBYTECODE: don't write .pyc files (same as -B).
PYTHONWARNINGS        : warning control; comma-separated -W filters.
PYTHONBREAKPOINT      : override sys.breakpointhook (default 'pdb.set_trace').
PYTHONUTF8            : force the interpreter into UTF-8 mode.
PYTHONNODEBUGRANGES   : disable PEP 657 column-precise tracebacks (no-op today).
PYTHONSAFEPATH        : same as -P option.
";

const HELP_XOPTIONS: &str = "\
The following implementation-specific options are available:
-X faulthandler        : enable faulthandler (no-op today).
-X dev                 : enable runtime checks helpful for development.
-X utf8                : enable UTF-8 mode for the interpreter.
-X tracemalloc         : start tracing Python memory allocations (no-op today).
-X importtime          : show how long each import takes (no-op today).
-X showrefcount        : output the total reference count (no-op today).
-X frozen_modules=on|off : whether frozen modules should be used.
-X no_debug_ranges     : disable PEP 657 ranges (no-op today).
-X pycache_prefix=PATH : redirect __pycache__ to PATH.
-X int_max_str_digits  : set sys.int_info.str_digits_check_threshold.
";

// Opt-in native crash diagnostics (`WEAVEPY_SEGV_BT`): macOS-only, because
// the raw `siginfo_t`/`ucontext_t` byte offsets below are the Darwin layouts.
#[cfg(target_os = "macos")]
extern "C" {
    fn signal(signum: i32, handler: usize) -> usize;
    fn sigaction(signum: i32, act: *const SigActionC, old: *mut SigActionC) -> i32;
    fn backtrace(array: *mut *mut std::ffi::c_void, size: i32) -> i32;
    fn backtrace_symbols_fd(array: *const *mut std::ffi::c_void, size: i32, fd: i32);
}

/// `struct sigaction` (macOS/BSD layout): an 8-byte handler pointer union,
/// a 4-byte `sigset_t` mask, and a 4-byte flags word.
#[cfg(target_os = "macos")]
#[repr(C)]
struct SigActionC {
    sa_sigaction: usize,
    sa_mask: u32,
    sa_flags: i32,
}

/// `SA_SIGINFO` — deliver the 3-argument handler so we can read `si_addr`.
#[cfg(target_os = "macos")]
const SA_SIGINFO: i32 = 0x0040;
/// Byte offset of `si_addr` within macOS `siginfo_t`
/// (`si_signo,si_errno,si_code,si_pid,si_uid,si_status` = 24 bytes precede it).
#[cfg(target_os = "macos")]
const SIGINFO_SI_ADDR_OFFSET: usize = 24;

/// Byte offset of the `mcontext_t` pointer within macOS `ucontext_t`
/// (`uc_onstack,uc_sigmask,uc_stack,uc_link,uc_mcsize` precede it).
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const UCONTEXT_MCONTEXT_OFFSET: usize = 48;
/// Byte offset of `__ss` (the ARM thread state) within macOS `mcontext64`
/// — it follows the 16-byte `__es` (ARM exception state).
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const MCONTEXT_SS_OFFSET: usize = 16;
/// Byte offset of `tp_name` (a `const char *`) within `PyTypeObject`.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const PYTYPEOBJECT_TP_NAME_OFFSET: usize = 0x18;

/// Read the C string at `p` (best-effort, capped) for signal-handler
/// diagnostics. Returns a lossy `String`; bails on an obviously-bad pointer
/// so we don't double-fault while already handling a crash.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
unsafe fn read_c_str_lossy(p: *const u8, cap: usize) -> String {
    if (p as usize) < 0x1000 {
        return String::from("<bad ptr>");
    }
    let mut bytes = Vec::new();
    for i in 0..cap {
        let b = unsafe { p.add(i).read() };
        if b == 0 {
            break;
        }
        bytes.push(b);
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(target_os = "macos")]
extern "C" fn weavepy_segv_backtrace(sig: i32, info: *const u8, ctx: *mut std::ffi::c_void) {
    // `ctx` (the interrupted-thread register file) is only decoded on arm64,
    // where the `mcontext64` layout below applies.
    #[cfg(not(target_arch = "aarch64"))]
    let _ = ctx;
    // The faulting memory address (`si_addr`) is the single most useful clue
    // for a native crash in a dlopen'd extension: a small value (`0x0`, `0x8`,
    // …) is a NULL-based field deref, a huge value a wild pointer. Printing it
    // turns an opaque `PyArray_*` frame into an actionable diagnosis.
    if !info.is_null() {
        let si_addr = unsafe {
            info.add(SIGINFO_SI_ADDR_OFFSET)
                .cast::<usize>()
                .read_unaligned()
        };
        eprintln!("\n=== WEAVEPY signal {sig} faulting address = 0x{si_addr:x} ===");
    }
    // Faulting register file (arm64): `pc` pinpoints the exact instruction and
    // `x0` is usually the receiver of a `Py_TYPE(x)->tp_field` chain. When the
    // crash is a NULL `tp_mro`/`tp_dict`/… deref, `x0` is still the live type
    // pointer, so decoding `x0->tp_name` names the offending type directly.
    #[cfg(target_arch = "aarch64")]
    if !ctx.is_null() {
        unsafe {
            let mctx = ctx
                .cast::<u8>()
                .add(UCONTEXT_MCONTEXT_OFFSET)
                .cast::<*const u8>()
                .read_unaligned();
            if !mctx.is_null() {
                let ss = mctx.add(MCONTEXT_SS_OFFSET);
                let x = |n: usize| ss.add(n * 8).cast::<u64>().read_unaligned();
                let pc = ss.add(256).cast::<u64>().read_unaligned();
                eprintln!(
                    "=== registers: pc=0x{pc:x} x0=0x{:x} x1=0x{:x} x8=0x{:x} x19=0x{:x} x20=0x{:x} ===",
                    x(0), x(1), x(8), x(19), x(20)
                );
                // Heuristic: for a `tp_*` NULL-field crash the type pointer is
                // in x0 (and often mirrored in x19/x20). Decode each as a
                // candidate `PyTypeObject*` and print its `tp_name`.
                for (reg, val) in [("x0", x(0)), ("x19", x(19)), ("x20", x(20))] {
                    let name_pp = (val as usize + PYTYPEOBJECT_TP_NAME_OFFSET) as *const *const u8;
                    if (val as usize) > 0x1000 {
                        let name = read_c_str_lossy(name_pp.read(), 64);
                        eprintln!("===   {reg} as PyTypeObject* -> tp_name = {name:?} ===");
                    }
                }
            }
        }
    }
    // Native (dladdr-based) backtrace first: it resolves frames inside a
    // dlopen'd `.so` (e.g. a Cython extension's static helpers) to their
    // real `module + symbol + offset`, which Rust's `std::backtrace`
    // mis-attributes to the nearest exported libsystem symbol.
    let mut frames: [*mut std::ffi::c_void; 96] = [std::ptr::null_mut(); 96];
    let n = unsafe { backtrace(frames.as_mut_ptr(), 96) };
    eprintln!("=== WEAVEPY signal {sig} native backtrace ===");
    unsafe { backtrace_symbols_fd(frames.as_ptr(), n, 2) };
    eprintln!("=== end native backtrace ===");
    let bt = std::backtrace::Backtrace::force_capture();
    eprintln!("=== WEAVEPY signal {sig} rust backtrace ===\n{bt}\n=== end backtrace ===");
    unsafe {
        signal(sig, 0);
    }
    std::process::abort();
}

fn main() -> ExitCode {
    #[cfg(target_os = "macos")]
    if std::env::var_os("WEAVEPY_SEGV_BT").is_some() {
        // `SA_SIGINFO` so the handler receives `siginfo_t` and can report the
        // faulting address; `signal()` alone would only pass the signal number.
        let act = SigActionC {
            sa_sigaction: weavepy_segv_backtrace as *const () as usize,
            sa_mask: 0,
            sa_flags: SA_SIGINFO,
        };
        unsafe {
            sigaction(11, &raw const act, std::ptr::null_mut()); // SIGSEGV
            sigaction(10, &raw const act, std::ptr::null_mut()); // SIGBUS
        }
    }
    // Undo Rust's pre-`main` `sanitize_standard_fds` (which re-opens any closed
    // std fd onto `/dev/null`) so an inherited-closed stdin/stdout/stderr stays
    // closed, matching CPython (`test_posix.test_close_file`). Must run before
    // any descriptor work.
    weavepy::vm::proc_init::restore_initial_std_fds();
    run_on_large_stack(main_dispatch)
}

/// WeavePy evaluates Python by recursive descent, so Python call depth
/// maps onto native (Rust) stack depth (see `crates/weavepy-vm/src/
/// recursion.rs`). Run the whole interpreter on a thread with a large
/// stack reserve so that `sys.setrecursionlimit` — enforced by the VM's
/// recursion guard (RFC 0037) — is what bounds recursion, rather than
/// the fixed OS main-thread stack (8 MiB on Linux/macOS). This makes the
/// behaviour uniform across platforms *and* build profiles: debug builds
/// have much larger per-activation stack frames than release, so without
/// this a default `setrecursionlimit(1000)` would overflow the native
/// stack in debug before the guard could fire. The reserve is committed
/// lazily by the OS, so it costs address space, not memory.
fn run_on_large_stack(entry: fn() -> ExitCode) -> ExitCode {
    const STACK_BYTES: usize = 1024 * 1024 * 1024; // 1 GiB reserve

    // The interpreter runs on the spawned `weavepy-main` thread, not the
    // process's initial OS thread (which only parks in `join()` below).
    // Block the asynchronous, process-directed signals (SIGINT, SIGALRM,
    // …) on this initial thread *before* spawning so a signal racing in
    // during startup can't be stolen by the soon-to-be-parked thread —
    // where it would merely trip the pending flag while the VM thread's
    // blocking syscall never gets EINTR (CPython's test_io SignalsTest
    // would then hang forever). The VM thread re-enables them for itself
    // first thing, making it the sole, deterministic delivery target.
    weavepy::vm::stdlib::signal_mod::block_async_signals_current_thread();

    let vm_entry = move || -> ExitCode {
        // Opt-in (`WEAVEPY_CRASH_BT`): register the native crash handler +
        // per-thread sigaltstack on the VM thread itself so a stack-overflow
        // SIGSEGV can be caught and reported (no-op stub on Windows).
        if std::env::var_os("WEAVEPY_CRASH_BT").is_some() {
            extern "C" {
                fn weavepy_install_crash_handler();
            }
            unsafe { weavepy_install_crash_handler() };
        }
        weavepy::vm::stdlib::signal_mod::unblock_async_signals_current_thread();
        // Arm SIGINT -> KeyboardInterrupt at startup (CPython does this during
        // interpreter init), so even scripts that never `import signal` raise
        // KeyboardInterrupt on ^C instead of being killed by the kernel default.
        weavepy::vm::stdlib::signal_mod::install_startup_dispositions();
        // Snapshot the OS-thread count *now* — on the VM thread, before any
        // user code can spawn `threading` workers or raw pthreads — so that a
        // later `os.fork()` can tell "single-threaded" (no warning) from
        // "multi-threaded" (CPython's fork `DeprecationWarning`). WeavePy runs
        // the interpreter off the parked process-initial thread, so the
        // quiescent process already has >1 OS thread; this baseline is what the
        // fork-warning check measures additional threads against.
        weavepy::vm::stdlib::os_process::capture_thread_baseline();
        entry()
    };

    match std::thread::Builder::new()
        .name("weavepy-main".to_owned())
        .stack_size(STACK_BYTES)
        .spawn(vm_entry)
    {
        Ok(handle) => handle.join().unwrap_or(ExitCode::FAILURE),
        // Extremely unlikely, but if the OS refuses the thread, fall back
        // to running on the current thread — restore signal delivery here
        // first since we blocked it above.
        Err(_) => {
            weavepy::vm::stdlib::signal_mod::unblock_async_signals_current_thread();
            weavepy::vm::stdlib::signal_mod::install_startup_dispositions();
            weavepy::vm::stdlib::os_process::capture_thread_baseline();
            entry()
        }
    }
}

fn main_dispatch() -> ExitCode {
    init_tracing();

    // `env::args()` panics on non-UTF-8 argv (bpo-35883's exact repro);
    // decode PEP 383-style instead, carrying undecodable bytes in the
    // PUA bridge window that `Interpreter::set_argv` maps back to
    // lone surrogates (RFC 0050).
    let raw: Vec<String> = weavepy::vm::os_args_bridged();

    // Multiprocessing spawn-child entry point. The parent passes
    // `--multiprocessing-fork` and an optional payload fd via
    // `WEAVEPY_MP_PAYLOAD_FD`; we hand off to
    // `multiprocessing._run_spawn_child()` which reads the pickled
    // task off the inherited fd and runs it.
    if raw.iter().any(|a| a == "--multiprocessing-fork") {
        return run_multiprocessing_child(&raw);
    }

    // Bare subcommand dispatch (e.g. `weavepy regrtest ...`) — must
    // run before clap, which would try to interpret the subcommand as
    // a positional `script` and trip on unknown flags after it.
    if raw.len() >= 2 && SUBCOMMANDS.contains(&raw[1].as_str()) {
        let sub = raw[1].clone();
        let rest: Vec<String> = std::iter::once(format!("weavepy {sub}"))
            .chain(raw.into_iter().skip(2))
            .collect();
        return match sub.as_str() {
            "regrtest" => match regrtest_cmd::run(rest) {
                Ok(code) => code,
                Err(err) => {
                    let mut stderr = io::stderr().lock();
                    let _ = writeln!(stderr, "weavepy regrtest: {err:#}");
                    ExitCode::from(1)
                }
            },
            _ => unreachable!(),
        };
    }

    match real_main() {
        Ok(code) => code,
        Err(err) => {
            if err.to_string() != DIAGNOSTIC_SENTINEL {
                let mut stderr = io::stderr().lock();
                let _ = writeln!(stderr, "weavepy: {err:#}");
            }
            ExitCode::from(1)
        }
    }
}

/// Split argv at the first `-c CMD` / `-m MODULE` / `script` / `-` / `--`
/// boundary so flags meant for the child program don't get re-parsed by
/// clap. Returns `(weavepy_args, mode, child_args)`.
///
/// `mode` is one of:
/// - `Some(("c", "<cmd>"))` — `-c CMD` was found.
/// - `Some(("m", "<mod>"))` — `-m MOD` was found.
/// - `Some(("s", "<path>"))` — a positional script was found.
/// - `Some(("-", ""))`     — `-` (stdin) was found.
/// - `None`                — interactive mode (no boundary).
fn split_argv(raw: Vec<String>) -> (Vec<String>, Option<(&'static str, String)>, Vec<String>) {
    let mut wp: Vec<String> = Vec::with_capacity(raw.len());
    let mut iter = raw.into_iter();
    if let Some(prog) = iter.next() {
        wp.push(prog);
    }
    while let Some(arg) = iter.next() {
        if arg == "--" {
            return (wp, None, iter.collect());
        }
        if arg == "-c" {
            let Some(cmd) = iter.next() else {
                argument_expected_error('c');
            };
            let rest: Vec<String> = iter.collect();
            return (wp, Some(("c", cmd)), rest);
        }
        if arg == "-m" {
            let Some(m) = iter.next() else {
                argument_expected_error('m');
            };
            let rest: Vec<String> = iter.collect();
            return (wp, Some(("m", m)), rest);
        }
        if arg.starts_with("-c") && arg.len() > 2 {
            let cmd = arg[2..].to_owned();
            let rest: Vec<String> = iter.collect();
            return (wp, Some(("c", cmd)), rest);
        }
        if arg.starts_with("-m") && arg.len() > 2 {
            let m = arg[2..].to_owned();
            let rest: Vec<String> = iter.collect();
            return (wp, Some(("m", m)), rest);
        }
        // Attached `-Xkey[=value]` / `-Wfilter` (CPython's own spelling —
        // `test_subprocess.test_encoding_warning` spawns `-Xwarn_default_encoding`):
        // normalise to the separate `-X key` form clap parses, so the option
        // reaches `sys._xoptions` / `sys.warnoptions`.
        if let Some(rest) = arg.strip_prefix("-X").filter(|r| !r.is_empty()) {
            wp.push("-X".to_owned());
            wp.push(rest.to_owned());
            continue;
        }
        if let Some(rest) = arg.strip_prefix("-W").filter(|r| !r.is_empty()) {
            wp.push("-W".to_owned());
            wp.push(rest.to_owned());
            continue;
        }
        // Clustered single-letter options where `-c`/`-m` follows some boolean
        // flags, e.g. `-uc CMD` == `-u -c CMD` and `-uIcCMD` == `-u -I -c CMD`
        // (CPython accepts this; `test_subprocess` spawns children as `-uc`).
        // The `c`/`m` consumes the rest of the cluster as its value, else the
        // next argv element.
        if arg.starts_with('-') && !arg.starts_with("--") && arg.len() > 2 {
            let body: Vec<char> = arg[1..].chars().collect();
            if let Some(pos) = body.iter().position(|&c| c == 'c' || c == 'm') {
                const BOOL_SHORT: &[char] = &[
                    'O', 'b', 'B', 'd', 'E', 'i', 'I', 'R', 'S', 's', 'q', 'P', 'u', 'v', 'x',
                ];
                if body[..pos].iter().all(|c| BOOL_SHORT.contains(c)) {
                    for &c in &body[..pos] {
                        wp.push(format!("-{c}"));
                    }
                    let kind = if body[pos] == 'c' { "c" } else { "m" };
                    let after: String = body[pos + 1..].iter().collect();
                    let value = if after.is_empty() {
                        iter.next()
                            .unwrap_or_else(|| argument_expected_error(body[pos]))
                    } else {
                        after
                    };
                    let rest: Vec<String> = iter.collect();
                    return (wp, Some((kind, value)), rest);
                }
            }
        }
        if arg == "-" {
            let rest: Vec<String> = iter.collect();
            return (wp, Some(("-", String::new())), rest);
        }
        // Value-taking flags: consume the following arg too, so it
        // isn't mistaken for the positional script (`-X opt script.py`).
        if arg == "-X" || arg == "-W" || arg == "--check-hash-based-pycs" {
            wp.push(arg);
            if let Some(value) = iter.next() {
                wp.push(value);
            }
            continue;
        }
        if !arg.starts_with('-') {
            // Positional script.
            let rest: Vec<String> = iter.collect();
            return (wp, Some(("s", arg)), rest);
        }
        wp.push(arg);
    }
    (wp, None, Vec::new())
}

fn real_main() -> Result<ExitCode> {
    let raw: Vec<String> = weavepy::vm::os_args_bridged();
    let (wp_argv, mode, child_argv) = split_argv(raw);
    // Re-parse the WeavePy-only slice with clap.
    let mut cli = Cli::parse_from(wp_argv);
    // Stuff `mode` back into the parsed Cli so the rest of real_main
    // sees a consistent view.
    match &mode {
        Some(("c", cmd)) => cli.command = Some(decode_command_arg(cmd)),
        Some(("m", m)) => cli.module = Some(m.clone()),
        // A script path may carry PEP 383-escaped bytes (PUA-bridged by
        // `os_args_bridged`); recover the OS-level bytes so the file
        // actually opens (RFC 0050).
        Some(("s", path)) => cli.script = Some(bridged_arg_to_pathbuf(path)),
        Some(("-", _)) => cli.script = Some(PathBuf::from("-")),
        _ => {}
    }
    cli.args = child_argv;

    if cli.help {
        print!("{HELP_BODY}");
        return Ok(ExitCode::SUCCESS);
    }
    if cli.help_env {
        print!("{HELP_ENV}");
        return Ok(ExitCode::SUCCESS);
    }
    if cli.help_xoptions {
        print!("{HELP_XOPTIONS}");
        return Ok(ExitCode::SUCCESS);
    }
    if cli.version {
        println!("WeavePy {VERSION}");
        return Ok(ExitCode::SUCCESS);
    }

    let env = if cli.isolated || cli.ignore_env {
        EnvOverrides::ignored()
    } else {
        EnvOverrides::from_env()
    };

    let mut flags = build_flags(&cli, &env);

    // Compose pythonpath from env (when honoured) plus -X variants.
    let mut extra_path: Vec<PathBuf> = env
        .pythonpath
        .iter()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect();

    // `WEAVEPY_CPYTHON_LIB` points at an external stdlib `Lib` directory
    // (the vendored CPython tree). Like a real interpreter that finds its
    // stdlib relative to the executable, this is part of the *default*
    // module search path: it is honoured even under `-I`/`-E` (it is not a
    // `PYTHON*` variable, so isolation does not strip it) so child
    // interpreters spawned via `sys.executable` — e.g. `assert_python_ok`,
    // `multiprocessing` spawn, `subprocess` re-execs — can still import the
    // stdlib and the `test` package. Unset in normal use, so this is a
    // no-op outside the conformance harness.
    if let Some(lib) = env::var_os("WEAVEPY_CPYTHON_LIB") {
        for part in env::split_paths(&lib) {
            if !part.as_os_str().is_empty() {
                extra_path.push(part);
            }
        }
    }

    if let Some(source) = cli.command.clone() {
        let mut argv = vec!["-c".to_owned()];
        argv.extend(cli.args.iter().cloned());
        // CPython's `-c` puts the *empty string* at `sys.path[0]` (an
        // '' entry means "current directory, resolved at import time"),
        // not a materialized cwd path —
        // `test_cmd_line_script.test_issue8202_dash_c_file_ignored`.
        let opts = RunOptions::new("<string>")
            .with_argv(argv)
            .with_extra_path(extra_path.drain(..))
            .with_script_dir("")
            .with_flags(flags.clone());
        // `-i` is handled inside `run_source_with_options`, which drops
        // into a namespace-sharing REPL after the program body.
        run_source_with_options(&source, &opts)?;
        return Ok(ExitCode::SUCCESS);
    }

    if let Some(module) = cli.module.clone() {
        let extra = cli.args.clone();
        run_module(&module, extra, &flags, &extra_path)?;
        return Ok(ExitCode::SUCCESS);
    }

    let script = cli.script.clone();
    let trailing = cli.args.clone();
    match script.as_deref() {
        Some(path) if path.as_os_str() == "-" => {
            run_stdin(trailing.clone(), &flags, &extra_path)?;
            Ok(ExitCode::SUCCESS)
        }
        Some(path) => {
            run_path(path, trailing.clone(), &flags, &extra_path)?;
            Ok(ExitCode::SUCCESS)
        }
        None => {
            // No script. CPython enters the REPL only when stdin is a
            // tty (or `-i` forces it); a piped stdin is read to EOF and
            // run as a program named `<stdin>` (`pymain_run_stdin` —
            // no banner, no `>>>` prompts, plain tracebacks).
            let stdin_is_tty = std::io::IsTerminal::is_terminal(&io::stdin());
            if stdin_is_tty || flags.inspect {
                flags.inspect = true;
                run_repl(flags, env.startup.as_deref(), trailing)?;
            } else {
                run_stdin(trailing, &flags, &extra_path)?;
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// CPython's `pymain_err_print` for an option missing its argument:
/// diagnostics + usage line on stderr, exit status 2.
fn argument_expected_error(opt: char) -> ! {
    eprintln!("Argument expected for the -{opt} option");
    eprintln!("usage: weavepy [option] ... [-c cmd | -m mod | file | -] [arg] ...");
    eprintln!("Try `weavepy -h' for more information.");
    std::process::exit(2);
}

/// A startup configuration error CPython reports through
/// `Py_ExitStatusException`: `Fatal Python error: <where>: <msg>`, exit 1.
fn config_fatal_error(whence: &str, msg: &str) -> ! {
    eprintln!("Fatal Python error: {whence}: {msg}");
    std::process::exit(1);
}

/// The value of the last `-X name[=value]` occurrence: `None` when the
/// option wasn't given, `Some(None)` for the bare form, `Some(Some(v))`
/// for `-X name=v`.
fn xoption_value<'a>(xoptions: &'a [String], name: &str) -> Option<Option<&'a str>> {
    xoptions.iter().rev().find_map(|x| {
        if x == name {
            Some(None)
        } else {
            x.strip_prefix(name)
                .and_then(|rest| rest.strip_prefix('='))
                .map(Some)
        }
    })
}

/// Parse + validate the PEP 0467 digit cap (`0` or `>= 640`), exiting
/// with CPython's `config_init_int_max_str_digits` fatal error otherwise.
fn parse_int_max_str_digits(value: &str, source: &str) -> i64 {
    match value.parse::<i64>() {
        Ok(n) if n == 0 || n >= 640 => n,
        _ => config_fatal_error(
            "config_init_int_max_str_digits",
            &format!("{source}: invalid limit; must be >= 640 or 0 for unlimited."),
        ),
    }
}

/// Compose the runtime [`InterpreterFlags`] from the CLI table and
/// the environment overrides. `-I` is the trump card.
fn build_flags(cli: &Cli, env: &EnvOverrides) -> InterpreterFlags {
    let isolated = cli.isolated;
    let ignore_env = cli.ignore_env || isolated;
    // Pin the per-process str/bytes hash salt before the interpreter
    // hashes anything (PEP 456 / `PYTHONHASHSEED`). `-R` re-enables
    // randomization, which is also the default when the var is unset.
    if !cli.hash_randomization {
        if let Some(seed) = env.hash_seed {
            weavepy::vm::object::set_hash_seed(seed);
        }
    }
    // `-X pycache_prefix[=PATH]` beats `PYTHONPYCACHEPREFIX` even when
    // given bare / with an empty value (which unsets the env prefix).
    let pycache_prefix = match xoption_value(&cli.xoptions, "pycache_prefix") {
        Some(v) => v.filter(|p| !p.is_empty()).map(str::to_owned),
        None => env.pycache_prefix.clone(),
    };
    let int_max_str_digits = match xoption_value(&cli.xoptions, "int_max_str_digits") {
        Some(Some(v)) => Some(parse_int_max_str_digits(v, "-X int_max_str_digits")),
        Some(None) => config_fatal_error(
            "config_init_int_max_str_digits",
            "-X int_max_str_digits: invalid limit; must be >= 640 or 0 for unlimited.",
        ),
        None => env
            .int_max_str_digits
            .as_deref()
            .map(|v| parse_int_max_str_digits(v, "PYTHONINTMAXSTRDIGITS")),
    };
    // `-X cpu_count=N|default` / `PYTHON_CPU_COUNT` (gh-109595).
    let cpu_count_raw = match xoption_value(&cli.xoptions, "cpu_count") {
        Some(Some(v)) => Some(v.to_owned()),
        Some(None) => config_fatal_error(
            "config_init_cpu_count",
            "-X cpu_count=n option: n is missing or invalid",
        ),
        None => env.cpu_count.clone(),
    };
    let cpu_count = cpu_count_raw.and_then(|raw| {
        if raw == "default" {
            None
        } else {
            match raw.parse::<i64>() {
                Ok(n) if n >= 1 => Some(n),
                _ => config_fatal_error(
                    "config_init_cpu_count",
                    "-X cpu_count=n option: n is missing or invalid",
                ),
            }
        }
    });
    // `-X gil` / `PYTHON_GIL` (PEP 703): only "1" is meaningful on a
    // build whose GIL can't be disabled; "0" is a startup fatal error.
    let gil = match xoption_value(&cli.xoptions, "gil") {
        Some(v) => v.map(str::to_owned),
        None => env.gil.clone(),
    };
    match gil.as_deref() {
        None | Some("1") => {}
        Some("0") => config_fatal_error(
            "config_read_gil",
            "Disabling the GIL is not supported by this build",
        ),
        Some(_) => config_fatal_error(
            "config_read_gil",
            "PYTHON_GIL / -X gil must be \"0\" or \"1\"",
        ),
    }
    let mut xoptions = cli.xoptions.clone();
    // `PYTHONDEVMODE` behaves like `-X dev` for `sys.flags.dev_mode`
    // (though CPython does *not* mirror it into `sys._xoptions`; the
    // duplicate key is harmless for our flag computation).
    if env.dev_mode && xoption_value(&xoptions, "dev").is_none() {
        xoptions.push("dev".to_owned());
    }
    InterpreterFlags {
        optimize: cli.optimize.max(env.optimize),
        dont_write_bytecode: cli.no_bytecode_write || env.dont_write_bytecode,
        inspect: cli.inspect_after || env.inspect,
        verbose: cli.verbose.max(env.verbose),
        no_site: cli.no_site,
        no_user_site: cli.no_user_site || env.no_user_site || isolated,
        ignore_environment: ignore_env,
        isolated,
        quiet: cli.quiet,
        unbuffered: cli.unbuffered || env.unbuffered,
        skip_first_line: cli.skip_first_line,
        bytes_warning: cli.bytes_warning,
        safe_path: cli.safe_path || env.safe_path || isolated,
        debug: cli.parser_debug.max(env.debug),
        xoptions,
        warning_filters: {
            let mut v = env.warning_filters.clone();
            v.extend(cli.warnings.iter().cloned());
            v
        },
        // `-R` re-enables randomization, trumping a fixed seed from
        // `PYTHONHASHSEED`.
        hash_seed: if cli.hash_randomization {
            None
        } else {
            env.hash_seed
        },
        io_encoding: env.io_encoding.clone(),
        io_errors: env.io_errors.clone(),
        utf8_mode: env.utf8_mode,
        pycache_prefix,
        int_max_str_digits,
        cpu_count,
    }
}

/// Subset of `PYTHON*` environment overrides we honour. Materialised
/// once per CLI invocation so each call site reads from a consistent
/// snapshot (env vars don't mutate mid-run).
#[derive(Debug, Default, Clone)]
struct EnvOverrides {
    pythonpath: Vec<String>,
    startup: Option<PathBuf>,
    optimize: u8,
    dont_write_bytecode: bool,
    inspect: bool,
    unbuffered: bool,
    verbose: u8,
    debug: u8,
    dev_mode: bool,
    no_user_site: bool,
    safe_path: bool,
    /// `PYTHONPYCACHEPREFIX` (PEP 552), losing to `-X pycache_prefix`.
    pycache_prefix: Option<String>,
    /// `PYTHONINTMAXSTRDIGITS`, raw (validated during flag composition
    /// so `-X int_max_str_digits` precedence applies first).
    int_max_str_digits: Option<String>,
    /// `PYTHON_CPU_COUNT`, raw (`"default"` or an integer ≥ 1).
    cpu_count: Option<String>,
    /// `PYTHON_GIL`, raw (`"0"` / `"1"`).
    gil: Option<String>,
    warning_filters: Vec<String>,
    hash_seed: Option<u32>,
    /// `PYTHONIOENCODING=encoding[:errors]`, split into its halves. Either
    /// part may be empty (`:errors` sets only the handler).
    io_encoding: Option<String>,
    io_errors: Option<String>,
    /// `PYTHONUTF8=0|1` (PEP 540). `None` when unset/empty; an invalid
    /// value is a startup fatal error (CPython `config_init_utf8_mode`).
    utf8_mode: Option<u8>,
}

impl EnvOverrides {
    fn from_env() -> Self {
        let mut o = Self::default();
        if let Ok(p) = env::var("PYTHONPATH") {
            o.pythonpath = p
                .split(if cfg!(windows) { ';' } else { ':' })
                .map(str::to_owned)
                .collect();
        }
        if let Ok(p) = env::var("PYTHONSTARTUP") {
            if !p.is_empty() {
                o.startup = Some(PathBuf::from(p));
            }
        }
        // CPython treats a `PYTHON*` variable set to the empty string as
        // unset (`config_get_env` / `_Py_GetEnv`); the int-valued ones
        // (`PYTHONOPTIMIZE`/`PYTHONVERBOSE`/`PYTHONDEBUG`) parse as an
        // integer with any non-numeric value meaning 1
        // (`test_cmd_line.test_sys_flags_set`).
        let nonempty = |name: &str| env::var(name).ok().filter(|v| !v.is_empty());
        let env_int = |name: &str| nonempty(name).map(|v| v.parse::<u8>().unwrap_or(1));
        if let Some(n) = env_int("PYTHONOPTIMIZE") {
            o.optimize = n;
        }
        o.dont_write_bytecode = nonempty("PYTHONDONTWRITEBYTECODE").is_some();
        o.inspect = nonempty("PYTHONINSPECT").is_some();
        o.unbuffered = nonempty("PYTHONUNBUFFERED").is_some();
        o.verbose = env_int("PYTHONVERBOSE").unwrap_or(0);
        // Unlike OPTIMIZE/VERBOSE, `PYTHONDEBUG` is a plain boolean env
        // in CPython (`config_get_env`, not the int-parsing variant):
        // any non-empty value — including "2" — means 1.
        o.debug = u8::from(nonempty("PYTHONDEBUG").is_some());
        o.dev_mode = nonempty("PYTHONDEVMODE").is_some();
        o.no_user_site = nonempty("PYTHONNOUSERSITE").is_some();
        o.safe_path = nonempty("PYTHONSAFEPATH").is_some();
        o.pycache_prefix = nonempty("PYTHONPYCACHEPREFIX");
        o.int_max_str_digits = nonempty("PYTHONINTMAXSTRDIGITS");
        o.cpu_count = nonempty("PYTHON_CPU_COUNT");
        o.gil = nonempty("PYTHON_GIL");
        if let Ok(w) = env::var("PYTHONWARNINGS") {
            o.warning_filters = w.split(',').map(str::to_owned).collect();
        }
        if let Ok(seed) = env::var("PYTHONHASHSEED") {
            if seed == "0" {
                o.hash_seed = Some(0);
            } else if let Ok(n) = seed.parse::<u32>() {
                o.hash_seed = Some(n);
            }
        }
        // `PYTHONIOENCODING=encoding[:errors]` (CPython): the first `:`
        // splits the codec from the error handler; either side may be
        // empty (`utf-8`, `:strict`, `ascii:backslashreplace`).
        if let Ok(spec) = env::var("PYTHONIOENCODING") {
            let (enc, errs) = match spec.split_once(':') {
                Some((e, h)) => (e, Some(h)),
                None => (spec.as_str(), None),
            };
            if !enc.is_empty() {
                o.io_encoding = Some(enc.to_owned());
            }
            if let Some(h) = errs {
                if !h.is_empty() {
                    o.io_errors = Some(h.to_owned());
                }
            }
        }
        // `PYTHONUTF8` (PEP 540): "1" enables UTF-8 mode, "0" disables it,
        // empty means unset; anything else is a startup fatal error
        // (CPython's `config_init_utf8_mode`).
        if let Ok(v) = env::var("PYTHONUTF8") {
            match v.as_str() {
                "" => {}
                "1" => o.utf8_mode = Some(1),
                "0" => o.utf8_mode = Some(0),
                other => {
                    eprintln!(
                        "Fatal Python error: init_utf8_mode: invalid PYTHONUTF8 environment \
                         variable value '{other}'"
                    );
                    std::process::exit(1);
                }
            }
        }
        o
    }

    fn ignored() -> Self {
        Self::default()
    }
}

/// Materialise the `-c` command text from its (possibly PUA-bridged)
/// argv transport, the way CPython's `pymain_run_command` receives it:
/// - clean text (the overwhelmingly common case) passes through;
/// - undecodable bytes under the `C`/`POSIX` locale decode to their
///   byte values (macOS/BSD `_Py_char2wchar` fallback — `test_cmd_line.
///   test_undecodable_code` expects `ascii("\xff")` to print `'\xff'`);
/// - otherwise the command cannot be represented and startup fails with
///   CPython's "Unable to decode the command from the command line".
fn decode_command_arg(cmd: &str) -> String {
    use weavepy::vm::object::Object;
    match weavepy::vm::argv_str_to_object(cmd) {
        Object::WStr(cps) => {
            let c_locale = ["LC_ALL", "LC_CTYPE", "LANG"]
                .iter()
                .find_map(|v| env::var(v).ok().filter(|s| !s.is_empty()))
                .is_none_or(|loc| loc == "C" || loc == "POSIX");
            if c_locale {
                cps.iter()
                    .map(|&cp| match cp {
                        0xDC80..=0xDCFF => char::from_u32(cp - 0xDC00).unwrap_or('\u{FFFD}'),
                        other => char::from_u32(other).unwrap_or('\u{FFFD}'),
                    })
                    .collect()
            } else {
                eprintln!("Unable to decode the command from the command line:");
                std::process::exit(1);
            }
        }
        Object::Str(s) => s.to_string(),
        _ => cmd.to_owned(),
    }
}

/// Rebuild a filesystem path from a (possibly PUA-bridged) argv string,
/// recovering the original OS bytes for PEP 383-escaped names.
fn bridged_arg_to_pathbuf(arg: &str) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        PathBuf::from(std::ffi::OsString::from_vec(
            weavepy::vm::bridged_arg_bytes(arg),
        ))
    }
    #[cfg(not(unix))]
    {
        PathBuf::from(arg)
    }
}

/// Escape a string into a Python single-quoted string literal.
fn quote_py_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn run_module(
    name: &str,
    args: Vec<String>,
    flags: &InterpreterFlags,
    extra_path: &[PathBuf],
) -> Result<()> {
    // Every `-m` goes through CPython's own entry point,
    // `runpy._run_module_as_main`: it imports parent packages (so the
    // target's relative imports resolve), redirects a package to its
    // `__main__` submodule, executes the target *in* the current
    // `__main__` namespace (so `-i -m timeit` leaves `Timer` visible to
    // the inspect REPL — `test_cmd_line.test_run_module_bug1764407`),
    // and reports a missing module the way CPython does
    // (`sys.exit("<exe>: Error while finding module specification …")`).
    //
    // `sys.argv[0]` starts as the literal `'-m'` — CPython's config
    // leaves the placeholder in place so code run *during the search*
    // (a parent package's `__init__`) sees it
    // (`test_cmd_line_script.test_issue8202`); `_run_module_as_main`
    // then swaps in the located file path before the target runs.
    let mut argv = vec!["-m".to_owned()];
    argv.extend(args.iter().cloned());
    let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut bootstrap = String::from("import runpy, sys\n");
    bootstrap.push_str(&format!(
        "runpy._run_module_as_main({})\n",
        quote_py_string(name)
    ));
    let opts = RunOptions::new(format!("<runpy:{name}>"))
        .with_argv(argv)
        .with_extra_path(extra_path.to_vec())
        .with_script_dir(cwd)
        .with_flags(flags.clone());
    run_source_with_options(&bootstrap, &opts)
}

/// Decode a script file's bytes per PEP 263 (BOM + coding cookie,
/// default strict UTF-8). On failure, print CPython's tokenizer-style
/// `SyntaxError` to stderr and exit 1 — like `python bad.py` does.
fn decode_script_source(bytes: &[u8], filename: &str) -> String {
    match weavepy::vm::decode_source_bytes(bytes, filename) {
        Ok(s) => s,
        Err(err) => {
            let msg = match &err {
                weavepy::vm::RuntimeError::PyException(pe) => pe.message(),
                other => other.to_string(),
            };
            // A NUL in the source: CPython reports the line the byte sits
            // on and echoes that line *truncated at the NUL*, with no
            // caret (`test_cmd_line_script.test_syntaxerror_null_bytes`).
            if let Some(pos) = bytes.iter().position(|&b| b == 0) {
                let line_no = bytes[..pos].iter().filter(|&&b| b == b'\n').count() + 1;
                let line_start = bytes[..pos]
                    .iter()
                    .rposition(|&b| b == b'\n')
                    .map_or(0, |i| i + 1);
                let line_text = String::from_utf8_lossy(&bytes[line_start..pos]);
                eprintln!("  File \"{filename}\", line {line_no}");
                let trimmed = line_text.trim_start();
                if !trimmed.is_empty() {
                    eprintln!("    {trimmed}");
                }
                eprintln!("SyntaxError: {msg}");
                std::process::exit(1);
            }
            eprintln!("  File \"{filename}\", line 1");
            eprintln!("SyntaxError: {msg}");
            std::process::exit(1);
        }
    }
}

fn run_path(
    path: &Path,
    extra: Vec<String>,
    flags: &InterpreterFlags,
    extra_path: &[PathBuf],
) -> Result<()> {
    // A directory or zipfile argument is executed as a module: CPython's
    // `pymain_run_module` adds the path itself to `sys.path[0]` and runs
    // `runpy._run_module_as_main("__main__")`, so `<dir>/__main__.py` (or the
    // zip's top-level `__main__`) becomes the program. (`python <dir>` /
    // `python app.zip`.)
    if path.is_dir() {
        return run_main_module_from_path(path, extra, flags, extra_path);
    }
    // CPython's `pymain_run_file`: an unopenable script prints
    // `<program>: can't open file '<abspath>': [Errno N] <strerror>`
    // (no traceback) and exits with status 2.
    //
    // The file is read exactly *once* and every content sniff (zip
    // magic, pyc magic) works off those bytes: a `/dev/fd/N` script
    // shares its seek offset with every other descriptor on the same
    // open file description, so a probe that consumed 4 magic bytes
    // would shear them off the program itself (GH-87235,
    // `test_cmd_line_script.test_script_as_dev_fd`).
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
            let program = env::args().next().unwrap_or_else(|| "weavepy".to_owned());
            let errno = e.raw_os_error().unwrap_or(2);
            eprintln!(
                "{program}: can't open file '{}': [Errno {errno}] {}",
                abs.display(),
                errno_message(errno)
            );
            std::process::exit(2);
        }
    };
    // `python app.zip`: the zip's top-level `__main__` becomes the program.
    if is_zip_bytes(&bytes) {
        return run_main_module_from_path(path, extra, flags, extra_path);
    }
    // A compiled-bytecode file (`.pyc`) given directly: CPython's
    // `pymain_run_file` detects the magic and runs the unmarshalled code
    // object as `__main__` (rather than trying to decode it as source).
    if is_pyc_bytes(&bytes) {
        return run_pyc_as_main(path, extra, flags, extra_path);
    }
    // CPython absolutizes the script path for `__main__.__file__` /
    // `co_filename` (getpath's `abspath(program_full_path)`), while
    // `sys.argv[0]` keeps the exact text the user typed
    // (`test_cmd_line_script.test_script_abspath`).
    let filename = std::path::absolute(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string();
    let source = decode_script_source(&bytes, &filename);
    let mut argv = vec![path.display().to_string()];
    argv.extend(extra);
    let script_dir = Path::new(&filename)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let opts = RunOptions::new(filename.clone())
        .with_argv(argv)
        .with_extra_path(extra_path.to_vec())
        .with_script_dir(script_dir)
        .with_flags(flags.clone());
    run_source_with_options(&source, &opts)
}

/// The OS `strerror` text for an errno, without the " (os error N)"
/// suffix `std::io::Error`'s Display appends.
fn errno_message(errno: i32) -> String {
    let s = io::Error::from_raw_os_error(errno).to_string();
    match s.find(" (os error ") {
        Some(i) => s[..i].to_owned(),
        None => s,
    }
}

/// CPython's `__pycache__`/legacy-`.pyc` magic (kept in sync with
/// `crates/weavepy-vm/src/pycache.rs` and `importlib.machinery.MAGIC_NUMBER`).
const PYC_MAGIC: [u8; 4] = [0xf3, 0x0d, 0x0d, 0x0a];

/// Whether `bytes` begins with the WeavePy bytecode magic + the 16-byte
/// `.pyc` header CPython writes (4 magic, 4 bit-field, 8 mtime/size or hash).
fn is_pyc_bytes(bytes: &[u8]) -> bool {
    bytes.len() >= 16 && bytes[..4] == PYC_MAGIC
}

/// Whether `bytes` begins with a zip signature (local-file/empty/spanned).
/// `python app.zip` runs the zip's top-level `__main__` via `zipimport`.
fn is_zip_bytes(bytes: &[u8]) -> bool {
    matches!(
        bytes.get(..4),
        Some([b'P', b'K', 0x03, 0x04] | [b'P', b'K', 0x05, 0x06] | [b'P', b'K', 0x07, 0x08])
    )
}

/// Run a directory or zipfile's top-level `__main__` as the program, with
/// `path` prepended to `sys.path` (CPython's directory/zipapp launch).
fn run_main_module_from_path(
    path: &Path,
    extra: Vec<String>,
    flags: &InterpreterFlags,
    extra_path: &[PathBuf],
) -> Result<()> {
    let path_str = path.display().to_string();
    let mut argv = vec![path_str.clone()];
    argv.extend(extra);
    // `alter_argv=False`: keep `sys.argv[0]` as the dir/zip path (CPython does
    // not rewrite it to the located `__main__` for directory/zip execution).
    let bootstrap =
        String::from("import runpy\nrunpy._run_module_as_main('__main__', alter_argv=False)\n");
    let opts = RunOptions::new(path_str)
        .with_argv(argv)
        .with_extra_path(extra_path.to_vec())
        .with_script_dir_always(path.to_path_buf())
        .with_flags(flags.clone());
    run_source_with_options(&bootstrap, &opts)
}

/// Run a `.pyc` file's marshalled code object as `__main__`, mirroring
/// CPython's `run_pyc_file`: `__main__.__file__` is the `.pyc` path and
/// `__spec__` stays `None` (a directly-run file is not an importable module),
/// so `multiprocessing` spawn reconstructs the child via `init_main_from_path`.
fn run_pyc_as_main(
    path: &Path,
    extra: Vec<String>,
    flags: &InterpreterFlags,
    extra_path: &[PathBuf],
) -> Result<()> {
    let path_str = path.display().to_string();
    let mut argv = vec![path_str.clone()];
    argv.extend(extra);
    let script_dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let quoted = quote_py_string(&path_str);
    let mut bootstrap = String::from("import sys, marshal\n");
    bootstrap.push_str(&format!("with open({quoted}, 'rb') as _f:\n"));
    bootstrap.push_str("    _data = _f.read()\n");
    bootstrap.push_str("_code = marshal.loads(_data[16:])\n");
    bootstrap.push_str("_g = sys.modules['__main__'].__dict__\n");
    bootstrap.push_str(&format!("_g['__file__'] = {quoted}\n"));
    bootstrap.push_str("_g['__cached__'] = None\n");
    bootstrap.push_str("_g['__spec__'] = None\n");
    // CPython's `pymain_run_file` on a `.pyc` installs a
    // `SourcelessFileLoader` as `__main__.__loader__`
    // (`test_cmd_line_script.test_script_compiled`).
    bootstrap.push_str("import importlib.machinery as _m\n");
    bootstrap.push_str(&format!(
        "_g['__loader__'] = _m.SourcelessFileLoader('__main__', {quoted})\n"
    ));
    bootstrap.push_str("del _m\n");
    bootstrap.push_str("del sys, marshal, _f, _data\n");
    bootstrap.push_str("exec(_code, _g)\n");
    let opts = RunOptions::new(path_str)
        .with_argv(argv)
        .with_extra_path(extra_path.to_vec())
        .with_script_dir(script_dir)
        .with_flags(flags.clone());
    run_source_with_options(&bootstrap, &opts)
}

fn run_stdin(extra: Vec<String>, flags: &InterpreterFlags, extra_path: &[PathBuf]) -> Result<()> {
    let mut buf = String::new();
    io::stdin()
        .read_to_string(&mut buf)
        .context("failed to read stdin")?;
    let mut argv = vec!["-".to_owned()];
    argv.extend(extra);
    // Like `-c`: stdin programs get `''` (cwd at import time) as
    // `sys.path[0]`, matching CPython's `pymain_run_stdin`.
    let opts = RunOptions::new("<stdin>")
        .with_argv(argv)
        .with_extra_path(extra_path.to_vec())
        .with_script_dir("")
        .with_flags(flags.clone());
    run_source_with_options(&buf, &opts)
}

fn run_source_with_options(source: &str, opts: &RunOptions) -> Result<()> {
    // CLI runs print uncaught exceptions CPython-style, through the
    // interpreter's `sys.excepthook` / `traceback` machinery (source
    // lines, carets, exception chains) while it is still alive.
    let opts = opts.clone().with_print_uncaught(true);
    // `-i` / `PYTHONINSPECT`: keep the interpreter alive and drop into
    // a REPL that shares the program's `__main__` namespace (CPython's
    // `pymain_repl`). An uncaught `SystemExit` is *ignored* — CPython's
    // `_Py_HandleSystemExit` says "Don't exit if -i flag was given"
    // (so `-i -m timeit`, whose main ends in `sys.exit(...)`, still
    // reaches the prompt); any other exception is printed first and
    // the prompt appears anyway.
    if opts.flags.inspect {
        let (interpreter, result) = weavepy::run_source_keep_interpreter(source, &opts);
        if let Err(err) = result {
            if err.system_exit_code().is_none() && !err.already_printed() {
                let mut stderr = io::stderr().lock();
                let diag = err.format(source, &opts.filename);
                let _ = stderr.write_all(diag.as_bytes());
            }
        }
        // No banner in inspect mode (CPython goes straight to `>>>`).
        let repl = repl::Repl::new(interpreter, true)?;
        return repl.run(None);
    }
    match weavepy::run_source_with_options(source, &opts) {
        Ok(()) => Ok(()),
        Err(err) => {
            // A `SystemExit` reaching the top level terminates the
            // process with its code and prints no traceback — exactly
            // like CPython. This is what makes `weavepy -m unittest`,
            // `-m test`, and bare `sys.exit()` behave as a drop-in.
            if let Some(code) = err.system_exit_code() {
                exit_with_system_exit(code);
            }
            if !err.already_printed() {
                let mut stderr = io::stderr().lock();
                let diag = err.format(source, &opts.filename);
                let _ = stderr.write_all(diag.as_bytes());
            }
            // bpo-1054041: an unhandled KeyboardInterrupt must terminate
            // the process *via* SIGINT (so a shell sees death-by-signal,
            // returncode == -SIGINT), after the traceback is printed.
            // This is CPython's `exit_sigint()` in Modules/main.c.
            if err.is_keyboard_interrupt() {
                exit_via_sigint();
            }
            anyhow::bail!(DIAGNOSTIC_SENTINEL);
        }
    }
}

/// Terminate the process the way CPython does when `SystemExit` reaches
/// the top level: `None` → 0, a bool/int → that code (masked to 8
/// bits), anything else → print `str(code)` to stderr and exit 1.
/// Never prints a traceback.
fn exit_with_system_exit(code: weavepy::vm::object::Object) -> ! {
    use weavepy::vm::object::Object;
    let _ = io::stdout().flush();
    let status: i32 = match code {
        Object::None => 0,
        Object::Bool(b) => i32::from(b),
        Object::Int(n) => (n & 0xFF) as i32,
        // A bare `raise SystemExit` (and `sys.exit()`) carries no
        // message; WeavePy models the empty payload as an empty string,
        // which means "no error" → exit 0, not a printed message.
        Object::Str(s) if s.is_empty() => 0,
        // An *int subclass* payload exits with its integer value —
        // `sys.exit(pytest.ExitCode.OK)` is an `enum.IntEnum`, and
        // CPython's `_Py_HandleSystemExit` does `PyLong_Check(value)`
        // which is subclass-inclusive (RFC 0055 WS5).
        Object::Instance(ref inst)
            if matches!(
                inst.native.get(),
                Some(Object::Int(_) | Object::Long(_) | Object::Bool(_))
            ) =>
        {
            (code.as_i64().unwrap_or(1) & 0xFF) as i32
        }
        // `sys.exit(SomeException('msg'))`: CPython prints `str(code)`.
        // The interpreter is already torn down, so mirror
        // `BaseException.__str__` from the args tuple directly
        // (`test_cmd_line_script.test_issue20500_exit_with_exception_value`).
        Object::Instance(inst) => {
            let args = inst
                .dict
                .borrow()
                .get(&weavepy::vm::object::DictKey(Object::from_static("args")))
                .cloned();
            let text = match args {
                Some(Object::Tuple(args)) => match args.len() {
                    0 => String::new(),
                    1 => args[0].to_str(),
                    _ => Object::Tuple(args).to_str(),
                },
                _ => Object::Instance(inst).to_str(),
            };
            let mut stderr = io::stderr().lock();
            let _ = writeln!(stderr, "{text}");
            1
        }
        other => {
            let mut stderr = io::stderr().lock();
            let _ = writeln!(stderr, "{}", other.to_str());
            1
        }
    };
    let _ = io::stderr().flush();
    std::process::exit(status);
}

/// Terminate via `SIGINT` under the default disposition, the way
/// CPython's `exit_sigint()` does when a `KeyboardInterrupt` goes
/// unhandled: reset `SIGINT` to `SIG_DFL` and `kill(getpid(), SIGINT)`
/// so the process dies *by the signal* (`returncode == -SIGINT`), which
/// is what shells and `subprocess` inspect. Falls back to exit code 130
/// (128 + SIGINT) if, impossibly, the signal doesn't terminate us.
#[cfg(unix)]
fn exit_via_sigint() -> ! {
    let _ = io::stdout().flush();
    let _ = io::stderr().flush();
    // Reset SIGINT to SIG_DFL, unblock it on this thread, and raise it
    // process-wide so we die *by the signal* (returncode == -SIGINT).
    weavepy::vm::stdlib::signal_mod::die_via_sigint();
    // Unreachable in practice; the signal terminates us above.
    std::process::exit(130);
}

#[cfg(not(unix))]
fn exit_via_sigint() -> ! {
    let _ = io::stdout().flush();
    let _ = io::stderr().flush();
    std::process::exit(0xC0_00_01_3A_u32 as i32);
}

fn run_repl(flags: InterpreterFlags, startup: Option<&Path>, argv: Vec<String>) -> Result<()> {
    let mut interpreter = weavepy::vm::Interpreter::default();
    interpreter.apply_run_options(&flags);
    if !argv.is_empty() {
        let mut a = vec![String::new()];
        a.extend(argv);
        interpreter.set_argv(a);
    } else {
        interpreter.set_argv(vec![String::new()]);
    }
    interpreter.prepend_path(env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    if !flags.no_site {
        let _ = interpreter.run_site();
    }
    let repl = repl::Repl::new(interpreter, flags.quiet)?;
    repl.run(startup)
}

fn init_tracing() {
    let filter = EnvFilter::try_from_env("WEAVEPY_LOG").unwrap_or_else(|_| EnvFilter::new("warn"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}
