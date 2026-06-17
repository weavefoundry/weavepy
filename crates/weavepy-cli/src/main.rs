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

/// Run a `weavepy --multiprocessing-fork` child. The parent has
/// arranged for the pickled task to arrive on the fd named in
/// `WEAVEPY_MP_PAYLOAD_FD` (defaults to `3`); we simply hand off to
/// `multiprocessing._run_spawn_child()`, which knows how to read the
/// payload, restore sys.path / cwd, and invoke the target callable.
///
/// The child's exit code is the value `_run_spawn_child()` returns,
/// stashed in a sentinel env var (`WEAVEPY_MP_EXIT_CODE`) so we can
/// re-read it from Rust without re-entering the VM.
fn run_multiprocessing_child() -> ExitCode {
    // `_run_spawn_child` invokes the worker target and then calls
    // `_multiprocessing._exit(code)` which `std::process::exit`s
    // directly — so this `Ok(())` arm is only reached when the worker
    // chose to fall through cleanly without an explicit exit (treated
    // as success).
    let snippet = "import multiprocessing, _multiprocessing\n\
                   _mp_code = multiprocessing._run_spawn_child()\n\
                   _multiprocessing._exit(int(_mp_code) if _mp_code is not None else 0)\n";
    let opts = RunOptions::new("<multiprocessing-fork>")
        .with_argv(vec!["weavepy".to_owned()])
        .with_flags(InterpreterFlags::default());
    match weavepy::run_source_with_options(snippet, &opts) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            let mut stderr = io::stderr().lock();
            let _ = writeln!(stderr, "{}", err.format(snippet, "<multiprocessing-fork>"));
            ExitCode::from(1)
        }
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
        return run_multiprocessing_child();
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
    // First look on the filesystem for a `<name>.py` / `<name>/__init__.py`.
    // If we find one, run it directly so the filename / __file__ honour
    // the host. Otherwise fall back to `runpy.run_module(...)` which can
    // resolve frozen built-in modules (`venv`, `pip`, `pdb`, …).
    let mut argv = vec![name.to_owned()];
    argv.extend(args.iter().cloned());
    let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let rel: PathBuf = name.split('.').collect();
    let mut search: Vec<PathBuf> = vec![cwd.clone()];
    search.extend(extra_path.iter().cloned());
    let on_disk = search.into_iter().find_map(|dir| {
        let m = dir.join(&rel).with_extension("py");
        if m.is_file() {
            return Some((m, false));
        }
        let init = dir.join(&rel).join("__init__.py");
        init.is_file().then_some((init, true))
    });
    if let Some((source_path, _)) = on_disk {
        let bytes = fs::read(&source_path)
            .with_context(|| format!("failed to read {}", source_path.display()))?;
        let filename = source_path.display().to_string();
        let source = decode_script_source(&bytes, &filename);
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
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let filename = path.display().to_string();
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
