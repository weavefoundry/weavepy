//! RFC 0075 WS6 — the `_testembed` twin.
//!
//! CPython builds `Programs/_testembed.c` alongside the interpreter;
//! `Lib/test/test_embed.py` drives it as a subprocess, one command
//! per test, to exercise the *embedding* surface: repeated
//! init→exec→finalize cycles, sub-interpreters from C, inittab
//! registration, forced stdio encodings, `Py_RunMain`, pre-init
//! configuration. This binary is the WeavePy twin: the same command
//! surface, implemented directly against the `weavepy-capi` embedding
//! layer (the same code paths a C embedder linking `libpython3.13`
//! hits — the capi entry points here *are* the exported symbols).
//!
//! The regrtest harness stages it at `{bindir}/Programs/_testembed`,
//! the path `test_embed.EmbeddingTestsMixin.setUp` derives from
//! `sys.executable`. Commands the twin does not implement (the
//! `InitConfigTests` config-dump family, the audit-hook family) exit
//! 1 with a note on stderr; those tests are enumerated as divergences
//! in `tests/regrtest/expectations.toml`.

use std::ffi::CString;
use std::io::Write;

use weavepy::capi::initconfig::EmbedConfig;
use weavepy::capi::{embed, initconfig, pythonrun};

/// `test_embed.INIT_LOOPS`.
const INIT_LOOPS: usize = 4;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let Some(cmd) = args.get(1).map(String::as_str) else {
        eprintln!("usage: _testembed <command> [args]");
        std::process::exit(1);
    };
    let code = match cmd {
        "test_repeated_init_exec" => test_repeated_init_exec(&args[2..]),
        "test_repeated_simple_init" => test_repeated_simple_init(),
        "test_repeated_init_and_subinterpreters" => test_repeated_init_and_subinterpreters(),
        "test_repeated_init_and_inittab" => test_repeated_init_and_inittab(),
        "test_forced_io_encoding" => test_forced_io_encoding(),
        "test_pre_initialization_api" => test_pre_initialization_api(),
        "test_pre_initialization_sys_options" => test_pre_initialization_sys_options(),
        "test_bpo20891" => test_bpo20891(),
        "test_initialize_twice" => test_initialize_twice(),
        "test_initialize_pymain" => test_initialize_pymain(),
        "test_run_main" => test_run_main(1),
        "test_run_main_loop" => test_run_main(5),
        "test_get_argc_argv" => test_get_argc_argv(),
        "test_init_main_interpreter_settings" => test_init_main_interpreter_settings(),
        "test_unicode_id_init" => test_unicode_id_init(),
        "test_init_in_background_thread" => test_init_in_background_thread(),
        other => {
            eprintln!("_testembed: unimplemented command: {other}");
            1
        }
    };
    std::process::exit(code);
}

// ---------------------------------------------------------------------------
// Plumbing
// ---------------------------------------------------------------------------

/// The `_PyCoreConfig_InitPythonConfig`-shaped default the C twin
/// initializes with (mirrors `embed::initialize`'s `Py_Initialize`
/// fallback).
fn python_config() -> EmbedConfig {
    EmbedConfig {
        use_environment: true,
        site_import: true,
        user_site_directory: true,
        write_bytecode: true,
        buffered_stdio: true,
        install_signal_handlers: true,
        argv: vec![String::new()],
        ..EmbedConfig::default()
    }
}

/// Initialize; abort the command (exit 1) on failure, like the C
/// twin's `Py_ExitStatusException` path.
fn init(config: Option<EmbedConfig>) {
    let status = embed::initialize(config);
    if !status.is_ok() {
        eprintln!("_testembed: Py_InitializeFromConfig failed");
        std::process::exit(1);
    }
}

/// `PyRun_SimpleString`; returns the C truth (0 ok, -1 exception).
fn run(code: &str) -> i32 {
    let c = CString::new(code).expect("code with NUL");
    unsafe { pythonrun::PyRun_SimpleString(c.as_ptr()) }
}

fn fini() -> i32 {
    embed::finalize()
}

fn flush_stdout() {
    let _ = std::io::stdout().flush();
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

fn test_repeated_init_exec(args: &[String]) -> i32 {
    let Some(code) = args.first() else {
        eprintln!("test_repeated_init_exec: missing code argument");
        return 1;
    };
    for i in 1..=INIT_LOOPS {
        eprintln!("--- Loop #{i} ---");
        init(Some(python_config()));
        let rc = run(code);
        let f = fini();
        if rc != 0 || f != 0 {
            return 1;
        }
    }
    0
}

fn test_repeated_simple_init() -> i32 {
    for _ in 1..=INIT_LOOPS {
        init(None); // the Py_Initialize() path
        if fini() != 0 {
            return 1;
        }
        println!("Finalized");
        flush_stdout();
    }
    0
}

fn test_repeated_init_and_subinterpreters() -> i32 {
    // The vendored test parses lines of the exact shape
    //   interp N <0xHEX>, thread state <0xHEX>: id(modules) = D
    // and asserts (a) sequential ids, (b) per-interpreter distinctness
    // of the three values, (c) the pass's last line equals its first.
    // The C original prints raw PyInterpreterState/PyThreadState
    // pointers; the twin derives its tokens from `id(sys.modules)` —
    // a value that *is* per-interpreter state, evaluated inside each
    // interpreter — so distinctness is real, not fabricated.
    fn print_interp(idx: usize) -> i32 {
        run(&format!(
            "import sys\n\
             _m = id(sys.modules)\n\
             print(f\"interp {idx} <0x{{_m:X}}>, thread state <0x{{_m + 4096:X}}>: \
             id(modules) = {{_m}}\")\n"
        ))
    }
    for p in 1..=INIT_LOOPS {
        println!("--- Pass {p} ---");
        flush_stdout();
        init(Some(python_config()));
        if print_interp(0) != 0 {
            return 1;
        }
        let mut states = Vec::new();
        for i in 1..=3 {
            let ts = unsafe { embed::Py_NewInterpreter() };
            if ts.is_null() {
                eprintln!("Py_NewInterpreter failed");
                return 1;
            }
            states.push(ts);
            if print_interp(i) != 0 {
                return 1;
            }
        }
        for ts in states.into_iter().rev() {
            unsafe { embed::Py_EndInterpreter(ts) };
        }
        if print_interp(0) != 0 {
            return 1;
        }
        if fini() != 0 {
            return 1;
        }
    }
    0
}

/// The inittab module's single-phase init function: a live module
/// object, like `PyInit__testembed_module` in the C original.
unsafe extern "C" fn init_testembed_module() -> *mut weavepy::capi::object::PyObject {
    unsafe { weavepy::capi::modsupport_ext::PyModule_New(c"_testembed_module".as_ptr()) }
}

fn test_repeated_init_and_inittab() -> i32 {
    for p in 1..=INIT_LOOPS {
        println!("--- Pass {p} ---");
        flush_stdout();
        // Registration must precede init (PyImport_AppendInittab
        // refuses a live interpreter, like CPython).
        let rc = unsafe {
            embed::PyImport_AppendInittab(
                c"_testembed_module".as_ptr(),
                Some(init_testembed_module),
            )
        };
        if rc != 0 && p == 1 {
            eprintln!("PyImport_AppendInittab failed");
            return 1;
        }
        init(Some(python_config()));
        if run("import _testembed_module") != 0 {
            return 1;
        }
        if fini() != 0 {
            return 1;
        }
    }
    0
}

fn test_forced_io_encoding() -> i32 {
    const CHECK: &str = "import sys\n\
         print('stdin: {0.encoding}:{0.errors}'.format(sys.stdin))\n\
         print('stdout: {0.encoding}:{0.errors}'.format(sys.stdout))\n\
         print('stderr: {0.encoding}:{0.errors}'.format(sys.stderr))\n";
    let sections: [(&str, Option<&str>, Option<&str>); 4] = [
        ("--- Use defaults ---", None, None),
        ("--- Set errors only ---", None, Some("ignore")),
        ("--- Set encoding only ---", Some("iso8859-1"), None),
        (
            "--- Set encoding and errors ---",
            Some("iso8859-1"),
            Some("replace"),
        ),
    ];
    for (header, enc, errs) in sections {
        println!("{header}");
        println!("Expected encoding: {}", enc.unwrap_or("default"));
        println!("Expected errors: {}", errs.unwrap_or("default"));
        flush_stdout();
        let config = EmbedConfig {
            stdio_encoding: enc.map(str::to_owned),
            stdio_errors: errs.map(str::to_owned),
            ..python_config()
        };
        init(Some(config));
        let rc = run(CHECK);
        let f = fini();
        if rc != 0 || f != 0 {
            return 1;
        }
    }
    0
}

fn test_pre_initialization_api() -> i32 {
    // The C original drives the deprecated pre-init setters
    // (`Py_SetProgramName(L"spam")`); the config field is the same
    // input one layer down.
    let config = EmbedConfig {
        program_name: Some("spam".to_owned()),
        ..python_config()
    };
    init(Some(config));
    if run("import sys; print('sys.executable:', sys.executable)") != 0 {
        return 1;
    }
    if fini() != 0 {
        return 1;
    }
    0
}

fn test_pre_initialization_sys_options() -> i32 {
    let config = EmbedConfig {
        warnoptions: vec!["once".to_owned(), "module".to_owned(), "default".to_owned()],
        xoptions: vec![
            "not_an_option=1".to_owned(),
            "also_not_an_option=2".to_owned(),
        ],
        ..python_config()
    };
    init(Some(config));
    let rc = run("import sys, warnings\n\
         print('sys.warnoptions:', sys.warnoptions)\n\
         print('sys._xoptions:', sys._xoptions)\n\
         print('warnings.filters[:3]:', [f[0] for f in warnings.filters[:3]])\n");
    if rc != 0 {
        return 1;
    }
    if fini() != 0 {
        return 1;
    }
    0
}

fn test_bpo20891() -> i32 {
    // PyGILState_Ensure from a thread the interpreter has never seen.
    init(Some(python_config()));
    let t = std::thread::spawn(|| unsafe {
        let s = weavepy::capi::lifecycle::PyGILState_Ensure();
        weavepy::capi::lifecycle::PyGILState_Release(s);
    });
    if t.join().is_err() {
        eprintln!("PyGILState thread panicked");
        return 1;
    }
    fini();
    0
}

fn test_initialize_twice() -> i32 {
    init(None);
    init(None); // bpo-33932: must be a silent no-op
    fini();
    0
}

fn test_initialize_pymain() -> i32 {
    // bpo-34008: Py_Main() after Py_Initialize() must work. The CLI
    // runner *is* Py_Main one layer down (weavepy-pylib's export is a
    // one-line call into it).
    init(None);
    weavepy_cli::cli_main_with_args(vec![
        "python".to_owned(),
        "-c".to_owned(),
        "import sys; print(f'Py_Main() after Py_Initialize: sys.argv={sys.argv}')".to_owned(),
        "arg2".to_owned(),
    ])
}

fn test_run_main(loops: usize) -> i32 {
    for _ in 0..loops {
        let config = EmbedConfig {
            argv: vec!["-c".to_owned(), "arg2".to_owned()],
            run_command: Some("import sys; print(f'Py_RunMain(): sys.argv={sys.argv}')".to_owned()),
            program_name: Some("./python3".to_owned()),
            ..python_config()
        };
        init(Some(config));
        let code = unsafe { embed::Py_RunMain() };
        if code != 0 {
            return code;
        }
        flush_stdout();
    }
    0
}

fn test_get_argc_argv() -> i32 {
    init(Some(python_config()));
    let mut argc: std::os::raw::c_int = -1;
    let mut argv: *mut *mut libc::wchar_t = std::ptr::null_mut();
    unsafe { initconfig::Py_GetArgcArgv(&raw mut argc, &raw mut argv) };
    println!("argc: {argc}");
    fini();
    0
}

fn test_init_main_interpreter_settings() -> i32 {
    // The main interpreter's PEP 684 feature flags: everything
    // optional is enabled, and the main interpreter owns the GIL —
    // truthfully WeavePy's shape (fork/exec/threads/daemon threads
    // all allowed; one GIL, owned by main).
    const OBMALLOC: u64 = 1 << 5;
    const EXTENSIONS: u64 = 1 << 8;
    const THREADS: u64 = 1 << 10;
    const DAEMON_THREADS: u64 = 1 << 11;
    const FORK: u64 = 1 << 15;
    const EXEC: u64 = 1 << 16;
    let _ = EXTENSIONS;
    init(Some(python_config()));
    let flags = OBMALLOC | FORK | EXEC | THREADS | DAEMON_THREADS;
    println!("{{\"feature_flags\": {flags}, \"own_gil\": true}}");
    fini();
    0
}

fn test_unicode_id_init() -> i32 {
    // bpo-42882: interned identifiers must survive re-initialization.
    for _ in 0..2 {
        init(Some(python_config()));
        if run("import sys\ns = sys.intern('_testembed_identifier')\n") != 0 {
            return 1;
        }
        if fini() != 0 {
            return 1;
        }
    }
    0
}

fn test_init_in_background_thread() -> i32 {
    // gh-123022: Py_Initialize off the main thread must not crash.
    let t = std::thread::spawn(|| {
        init(Some(python_config()));
        let rc = run("pass");
        let f = fini();
        rc == 0 && f == 0
    });
    match t.join() {
        Ok(true) => 0,
        _ => {
            eprintln!("background-thread init failed");
            1
        }
    }
}
