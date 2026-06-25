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
    #[arg(short = 'V', long = "version", action = ArgAction::SetTrue)]
    version: bool,

    /// Print this help and exit.
    #[arg(short = 'h', long = "help", action = ArgAction::SetTrue)]
    help: bool,

    /// Print the help-env summary (which `PYTHON*` vars are honoured) and exit.
    #[arg(long = "help-env", action = ArgAction::SetTrue)]
    help_env: bool,

    /// Print the help-xoptions summary and exit.
    #[arg(long = "help-xoptions", action = ArgAction::SetTrue)]
    help_xoptions: bool,

    /// Optimisation level. `-O` once, `-OO` twice.
    #[arg(short = 'O', action = ArgAction::Count)]
    optimize: u8,

    /// `bytes`/`str` comparison warnings. `-b` once warns, `-bb` errors.
    #[arg(short = 'b', action = ArgAction::Count)]
    bytes_warning: u8,

    /// Don't write `.pyc` files.
    #[arg(short = 'B', action = ArgAction::SetTrue)]
    no_bytecode_write: bool,

    /// Parser debug output (no-op stub today).
    #[arg(short = 'd', action = ArgAction::SetTrue)]
    parser_debug: bool,

    /// Ignore all `PYTHON*` environment variables.
    #[arg(short = 'E', action = ArgAction::SetTrue)]
    ignore_env: bool,

    /// Drop into the REPL after running the script / module / command.
    #[arg(short = 'i', action = ArgAction::SetTrue)]
    inspect_after: bool,

    /// Isolated mode: implies `-E -s` and sets `sys.flags.isolated`.
    #[arg(short = 'I', action = ArgAction::SetTrue)]
    isolated: bool,

    /// Don't run `site.main()` on interpreter startup.
    #[arg(short = 'S', action = ArgAction::SetTrue)]
    no_site: bool,

    /// Don't add the user site-packages to `sys.path`.
    #[arg(short = 's', action = ArgAction::SetTrue)]
    no_user_site: bool,

    /// Suppress the REPL banner.
    #[arg(short = 'q', action = ArgAction::SetTrue)]
    quiet: bool,

    /// Don't prepend the script dir / cwd to `sys.path`.
    #[arg(short = 'P', action = ArgAction::SetTrue)]
    safe_path: bool,

    /// Force stdout/stderr unbuffered.
    #[arg(short = 'u', action = ArgAction::SetTrue)]
    unbuffered: bool,

    /// Verbose imports.
    #[arg(short = 'v', action = ArgAction::Count)]
    verbose: u8,

    /// Skip the first source line (shebang trick).
    #[arg(short = 'x', action = ArgAction::SetTrue)]
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

fn main() -> ExitCode {
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

    let raw: Vec<String> = env::args().collect();

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
            let cmd = iter.next().unwrap_or_default();
            let rest: Vec<String> = iter.collect();
            return (wp, Some(("c", cmd)), rest);
        }
        if arg == "-m" {
            let m = iter.next().unwrap_or_default();
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
                    'O', 'b', 'B', 'd', 'E', 'i', 'I', 'S', 's', 'q', 'P', 'u', 'v', 'x',
                ];
                if body[..pos].iter().all(|c| BOOL_SHORT.contains(c)) {
                    for &c in &body[..pos] {
                        wp.push(format!("-{c}"));
                    }
                    let kind = if body[pos] == 'c' { "c" } else { "m" };
                    let after: String = body[pos + 1..].iter().collect();
                    let value = if after.is_empty() {
                        iter.next().unwrap_or_default()
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
    let raw: Vec<String> = env::args().collect();
    let (wp_argv, mode, child_argv) = split_argv(raw);
    // Re-parse the WeavePy-only slice with clap.
    let mut cli = Cli::parse_from(wp_argv);
    // Stuff `mode` back into the parsed Cli so the rest of real_main
    // sees a consistent view.
    match &mode {
        Some(("c", cmd)) => cli.command = Some(cmd.clone()),
        Some(("m", m)) => cli.module = Some(m.clone()),
        Some(("s", path)) => cli.script = Some(PathBuf::from(path)),
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
        let opts = RunOptions::new("<string>")
            .with_argv(argv)
            .with_extra_path(extra_path.drain(..))
            .with_script_dir(env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
            .with_flags(flags.clone());
        run_source_with_options(&source, &opts)?;
        if flags.inspect {
            run_repl(flags, env.startup.as_deref(), Vec::new())?;
        }
        return Ok(ExitCode::SUCCESS);
    }

    if let Some(module) = cli.module.clone() {
        let extra = cli.args.clone();
        run_module(&module, extra, &flags, &extra_path)?;
        if flags.inspect {
            run_repl(flags, env.startup.as_deref(), Vec::new())?;
        }
        return Ok(ExitCode::SUCCESS);
    }

    let script = cli.script.clone();
    let trailing = cli.args.clone();
    match script.as_deref() {
        Some(path) if path.as_os_str() == "-" => {
            run_stdin(trailing.clone(), &flags, &extra_path)?;
            if flags.inspect {
                run_repl(flags, env.startup.as_deref(), Vec::new())?;
            }
            Ok(ExitCode::SUCCESS)
        }
        Some(path) => {
            run_path(path, trailing.clone(), &flags, &extra_path)?;
            if flags.inspect {
                run_repl(flags, env.startup.as_deref(), Vec::new())?;
            }
            Ok(ExitCode::SUCCESS)
        }
        None => {
            // No script — interactive mode. Honour `-i`'s implicit
            // "interactive after" by always going to the REPL here.
            flags.inspect = true;
            run_repl(flags, env.startup.as_deref(), trailing)?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// Compose the runtime [`InterpreterFlags`] from the CLI table and
/// the environment overrides. `-I` is the trump card.
fn build_flags(cli: &Cli, env: &EnvOverrides) -> InterpreterFlags {
    let isolated = cli.isolated;
    let ignore_env = cli.ignore_env || isolated;
    let mut flags = InterpreterFlags {
        optimize: cli.optimize.max(env.optimize),
        dont_write_bytecode: cli.no_bytecode_write || env.dont_write_bytecode,
        inspect: cli.inspect_after || env.inspect,
        verbose: cli.verbose > 0 || env.verbose,
        no_site: cli.no_site,
        no_user_site: cli.no_user_site || env.no_user_site || isolated,
        ignore_environment: ignore_env,
        isolated,
        quiet: cli.quiet,
        unbuffered: cli.unbuffered || env.unbuffered,
        skip_first_line: cli.skip_first_line,
        bytes_warning: cli.bytes_warning,
        safe_path: cli.safe_path || env.safe_path || isolated,
        debug: cli.parser_debug,
        xoptions: cli.xoptions.clone(),
        warning_filters: {
            let mut v = env.warning_filters.clone();
            v.extend(cli.warnings.iter().cloned());
            v
        },
        hash_seed: env.hash_seed,
        io_encoding: env.io_encoding.clone(),
        io_errors: env.io_errors.clone(),
    };
    if cli.optimize == 0 && env.optimize > 0 {
        flags.optimize = env.optimize;
    }
    flags
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
    verbose: bool,
    no_user_site: bool,
    safe_path: bool,
    warning_filters: Vec<String>,
    hash_seed: Option<u32>,
    /// `PYTHONIOENCODING=encoding[:errors]`, split into its halves. Either
    /// part may be empty (`:errors` sets only the handler).
    io_encoding: Option<String>,
    io_errors: Option<String>,
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
        if let Ok(n) = env::var("PYTHONOPTIMIZE") {
            o.optimize = n.parse().unwrap_or(1);
        }
        if env::var_os("PYTHONDONTWRITEBYTECODE").is_some() {
            o.dont_write_bytecode = true;
        }
        if env::var_os("PYTHONINSPECT").is_some() {
            o.inspect = true;
        }
        if env::var_os("PYTHONUNBUFFERED").is_some() {
            o.unbuffered = true;
        }
        if env::var_os("PYTHONVERBOSE").is_some() {
            o.verbose = true;
        }
        if env::var_os("PYTHONNOUSERSITE").is_some() {
            o.no_user_site = true;
        }
        if env::var_os("PYTHONSAFEPATH").is_some() {
            o.safe_path = true;
        }
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
        o
    }

    fn ignored() -> Self {
        Self::default()
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
    // First look on the filesystem for a top-level `<name>.py`. A single-file
    // module has no `__main__` redirect and no parent package to initialise,
    // so we can run it directly and let the filename / `__file__` honour the
    // host source. Packages are deliberately NOT short-circuited here: CPython's
    // `python -m pkg` never executes `pkg/__init__.py` as `__main__`, it
    // redirects to `pkg.__main__` (importing `pkg` first so the target's
    // relative imports resolve). Running `__init__.py` directly breaks any
    // `from . import ...` in the package body (e.g. `zipfile`). So packages —
    // and everything else — fall through to the `runpy` path below, which
    // performs that redirect faithfully and also resolves frozen modules.
    let mut argv = vec![name.to_owned()];
    argv.extend(args.iter().cloned());
    let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let rel: PathBuf = name.split('.').collect();
    let mut search: Vec<PathBuf> = vec![cwd.clone()];
    search.extend(extra_path.iter().cloned());
    // Only a top-level, non-dotted name with a matching `<name>.py` takes the
    // fast path; a dotted `-m pkg.mod` needs `runpy` to import the parent
    // package first, and a package directory must redirect to `__main__`.
    let on_disk_module = if name.contains('.') {
        None
    } else {
        search.iter().find_map(|dir| {
            let m = dir.join(&rel).with_extension("py");
            m.is_file().then_some(m)
        })
    };
    if let Some(source_path) = on_disk_module {
        let bytes = fs::read(&source_path)
            .with_context(|| format!("failed to read {}", source_path.display()))?;
        let filename = source_path.display().to_string();
        let source = decode_script_source(&bytes, &filename);
        // CPython's `python -m mod` runs through `runpy._run_module_as_main`,
        // which sets `sys.argv[0]` to the module's resolved *file path*, not the
        // bare module name. Programs derive identity from it — e.g. argparse's
        // default `prog` is `os.path.basename(sys.argv[0])`, so `-m calendar -h`
        // must report `calendar.py`, not `calendar`. Mirror that here on the
        // single-file fast path (the `runpy` path below already does so via
        // `alter_sys=True`).
        argv[0] = filename.clone();
        let opts = RunOptions::new(filename.clone())
            .with_argv(argv)
            .with_extra_path(extra_path.to_vec())
            .with_script_dir(cwd)
            .with_flags(flags.clone());
        return run_source_with_options(&source, &opts);
    }
    // Frozen / built-in module path — delegate to runpy. The
    // bootstrap is a tiny snippet that imports runpy and asks it to
    // run the requested module as `__main__`. We make the host argv
    // visible up front so the loaded module's `sys.argv` matches
    // CPython's `python -m`.
    let mut bootstrap = String::from("import runpy, sys\n");
    bootstrap.push_str("try:\n");
    bootstrap.push_str(&format!(
        "    runpy.run_module({}, run_name='__main__', alter_sys=True)\n",
        quote_py_string(name)
    ));
    bootstrap.push_str("except ImportError as e:\n");
    bootstrap.push_str(&format!(
        "    sys.stderr.write(\"weavepy: No module named '{}': \" + str(e) + \"\\n\")\n",
        name
    ));
    bootstrap.push_str("    sys.exit(1)\n");
    let _ = args;
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
    if path.is_dir() || path_is_zipfile(path) {
        return run_main_module_from_path(path, extra, flags, extra_path);
    }
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let filename = path.display().to_string();
    // A compiled-bytecode file (`.pyc`) given directly: CPython's
    // `pymain_run_file` detects the magic and runs the unmarshalled code
    // object as `__main__` (rather than trying to decode it as source).
    if is_pyc_bytes(&bytes) {
        return run_pyc_as_main(path, extra, flags, extra_path);
    }
    let source = decode_script_source(&bytes, &filename);
    let mut argv = vec![filename.clone()];
    argv.extend(extra);
    let script_dir = path
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

/// CPython's `__pycache__`/legacy-`.pyc` magic (kept in sync with
/// `crates/weavepy-vm/src/pycache.rs` and `importlib.machinery.MAGIC_NUMBER`).
const PYC_MAGIC: [u8; 4] = [0xf3, 0x0d, 0x0d, 0x0a];

/// Whether `bytes` begins with the WeavePy bytecode magic + the 16-byte
/// `.pyc` header CPython writes (4 magic, 4 bit-field, 8 mtime/size or hash).
fn is_pyc_bytes(bytes: &[u8]) -> bool {
    bytes.len() >= 16 && bytes[..4] == PYC_MAGIC
}

/// Whether `path` is a zip archive (local-file/empty/spanned signatures).
/// `python app.zip` runs the zip's top-level `__main__` via `zipimport`.
fn path_is_zipfile(path: &Path) -> bool {
    use std::io::Read;
    let Ok(mut f) = fs::File::open(path) else {
        return false;
    };
    let mut magic = [0u8; 4];
    if f.read_exact(&mut magic).is_err() {
        return false;
    }
    matches!(
        magic,
        [b'P', b'K', 0x03, 0x04] | [b'P', b'K', 0x05, 0x06] | [b'P', b'K', 0x07, 0x08]
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
        .with_script_dir(path.to_path_buf())
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
    let opts = RunOptions::new("<stdin>")
        .with_argv(argv)
        .with_extra_path(extra_path.to_vec())
        .with_script_dir(env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
        .with_flags(flags.clone());
    run_source_with_options(&buf, &opts)
}

fn run_source_with_options(source: &str, opts: &RunOptions) -> Result<()> {
    // CLI runs print uncaught exceptions CPython-style, through the
    // interpreter's `sys.excepthook` / `traceback` machinery (source
    // lines, carets, exception chains) while it is still alive.
    let opts = opts.clone().with_print_uncaught(true);
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
