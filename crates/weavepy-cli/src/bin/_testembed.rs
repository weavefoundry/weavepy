//! RFC 0075 WS6 — the `_testembed` twin.
//!
//! CPython builds `Programs/_testembed.c` alongside the interpreter;
//! `Lib/test/test_embed.py` drives it as a subprocess, one command
//! per test, to exercise the *embedding* surface: repeated
//! init→exec→finalize cycles, sub-interpreters from C, inittab
//! registration, forced stdio encodings, `Py_RunMain`, pre-init
//! configuration. This binary is the WeavePy twin: the same command
//! surface, implemented directly against the `weavepy-capi` embedding
//! layer (the same code paths a C embedder linking `libpython3.14`
//! hits — the capi entry points here *are* the exported symbols).
//!
//! The regrtest harness stages it at `{bindir}/Programs/_testembed`,
//! the path `test_embed.EmbeddingTestsMixin.setUp` derives from
//! `sys.executable`. Commands the twin does not implement (the
//! `InitConfigTests` config-dump family, the audit-hook family) exit
//! 1 with a note on stderr; those tests are enumerated as divergences
//! in `tests/regrtest/expectations.toml`.

use std::ffi::{CStr, CString};
use std::io::Write;
use std::os::raw::c_char;

use weavepy::capi::initconfig::EmbedConfig;
use weavepy::capi::{embed, initconfig, pep741, pythonrun};

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
        "test_init_run_main_code_exitcode" => test_init_run_main_code_exitcode(&args[2..]),
        "test_init_run_main_script_exitcode" => test_init_run_main_script_exitcode(&args[2..]),
        "test_init_run_main_module_exitcode" => test_init_run_main_module_exitcode(&args[2..]),
        "test_init_run_main_interactive_exitcode" => test_init_run_main_interactive_exitcode(),
        "test_initconfig_get_api" => test_initconfig_get_api(),
        "test_initconfig_exit" => test_initconfig_exit(),
        "test_initconfig_module" => test_initconfig_module(),
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
    let Some(first) = args.first() else {
        eprintln!("test_repeated_init_exec: missing code argument");
        return 1;
    };
    // 3.14's `_testembed.c`: with several CODE arguments there is one loop
    // per argument, each running its own snippet (test_embed's
    // test_static_types_inherited_slots passes two and expects exactly
    // two `--- Loop #N ---` blocks); a single CODE runs INIT_LOOPS times.
    let loops = if args.len() > 1 {
        args.len()
    } else {
        INIT_LOOPS
    };
    for i in 1..=loops {
        eprintln!("--- Loop #{i} ---");
        let code = if args.len() > 1 { &args[i - 1] } else { first };
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

/// 3.14 `test_init_run_main_exitcode`: `Py_RunMain()` over a parsed
/// command line whose program raises `SystemExit(123)` must *return*
/// 123 (not `Py_Exit()` the process), so the trailer line reaches
/// stdout.
fn test_init_run_main_exitcode(config: EmbedConfig) -> i32 {
    init(Some(config));
    let exitcode = unsafe { embed::Py_RunMain() };
    if exitcode != 123 {
        eprintln!("Py_RunMain() returned {exitcode}, expected 123");
        return 1;
    }
    println!("ok! Py_RunMain() returned 123");
    0
}

fn test_init_run_main_code_exitcode(args: &[String]) -> i32 {
    let Some(code) = args.first() else {
        eprintln!("test_init_run_main_code_exitcode: missing CODE argument");
        return 1;
    };
    test_init_run_main_exitcode(EmbedConfig {
        argv: vec!["-c".to_owned()],
        run_command: Some(format!("{code}\n")),
        program_name: Some("./python3".to_owned()),
        ..python_config()
    })
}

fn test_init_run_main_script_exitcode(args: &[String]) -> i32 {
    let Some(filename) = args.first() else {
        eprintln!("test_init_run_main_script_exitcode: missing FILENAME argument");
        return 1;
    };
    test_init_run_main_exitcode(EmbedConfig {
        argv: vec![filename.clone()],
        run_filename: Some(filename.clone()),
        program_name: Some("./python3".to_owned()),
        ..python_config()
    })
}

fn test_init_run_main_module_exitcode(args: &[String]) -> i32 {
    let Some(module) = args.first() else {
        eprintln!("test_init_run_main_module_exitcode: missing MODULE argument");
        return 1;
    };
    test_init_run_main_exitcode(EmbedConfig {
        argv: vec!["-m".to_owned()],
        run_module: Some(module.clone()),
        program_name: Some("./python3".to_owned()),
        ..python_config()
    })
}

fn test_init_run_main_interactive_exitcode() -> i32 {
    // `python3 -i` over piped stdin (and `$PYTHONSTARTUP`, for
    // test_init_run_main_startup_exitcode).
    test_init_run_main_exitcode(EmbedConfig {
        argv: vec![String::new()],
        inspect: true,
        program_name: Some("./python3".to_owned()),
        ..python_config()
    })
}

// ---------------------------------------------------------------------------
// PEP 741 `PyInitConfig` (3.14 `test_initconfig_*`)
// ---------------------------------------------------------------------------

const PROGRAM_NAME_UTF8: &CStr = c"./_testembed";

fn initconfig_getint(config: *mut pep741::PyInitConfig, name: &CStr) -> i64 {
    let mut value: i64 = 0;
    let rc = unsafe { pep741::PyInitConfig_GetInt(config, name.as_ptr(), &raw mut value) };
    assert_eq!(rc, 0, "PyInitConfig_GetInt({name:?})");
    value
}

/// The C twin's `assert(...)` body: every probe is a hard check.
fn test_initconfig_get_api() -> i32 {
    let config = pep741::PyInitConfig_Create();
    if config.is_null() {
        println!("Init allocation error");
        return 1;
    }
    unsafe {
        // PyInitConfig_HasOption()
        assert_eq!(
            pep741::PyInitConfig_HasOption(config, c"verbose".as_ptr()),
            1
        );
        assert_eq!(
            pep741::PyInitConfig_HasOption(config, c"utf8_mode".as_ptr()),
            1
        );
        assert_eq!(
            pep741::PyInitConfig_HasOption(config, c"non-existent".as_ptr()),
            0
        );

        // PyInitConfig_GetInt()
        assert_eq!(initconfig_getint(config, c"dev_mode"), 0);
        assert_eq!(
            pep741::PyInitConfig_SetInt(config, c"dev_mode".as_ptr(), 1),
            0
        );
        assert_eq!(initconfig_getint(config, c"dev_mode"), 1);

        // PyInitConfig_GetInt() on a PyPreConfig option
        assert_eq!(initconfig_getint(config, c"utf8_mode"), 0);
        assert_eq!(
            pep741::PyInitConfig_SetInt(config, c"utf8_mode".as_ptr(), 1),
            0
        );
        assert_eq!(initconfig_getint(config, c"utf8_mode"), 1);

        // PyInitConfig_GetStr()
        let mut s: *mut c_char = std::ptr::null_mut();
        assert_eq!(
            pep741::PyInitConfig_GetStr(config, c"program_name".as_ptr(), &raw mut s),
            0
        );
        assert!(s.is_null());
        assert_eq!(
            pep741::PyInitConfig_SetStr(
                config,
                c"program_name".as_ptr(),
                PROGRAM_NAME_UTF8.as_ptr()
            ),
            0
        );
        assert_eq!(
            pep741::PyInitConfig_GetStr(config, c"program_name".as_ptr(), &raw mut s),
            0
        );
        assert_eq!(CStr::from_ptr(s), PROGRAM_NAME_UTF8);
        libc::free(s.cast());

        // PyInitConfig_GetStrList() and PyInitConfig_FreeStrList()
        let mut length: usize = 0;
        let mut items: *mut *mut c_char = std::ptr::null_mut();
        assert_eq!(
            pep741::PyInitConfig_GetStrList(
                config,
                c"xoptions".as_ptr(),
                &raw mut length,
                &raw mut items
            ),
            0
        );
        assert_eq!(length, 0);

        let xoptions: [*const c_char; 1] = [c"faulthandler".as_ptr()];
        assert_eq!(
            pep741::PyInitConfig_SetStrList(
                config,
                c"xoptions".as_ptr(),
                xoptions.len(),
                xoptions.as_ptr()
            ),
            0
        );
        assert_eq!(
            pep741::PyInitConfig_GetStrList(
                config,
                c"xoptions".as_ptr(),
                &raw mut length,
                &raw mut items
            ),
            0
        );
        assert_eq!(length, 1);
        assert_eq!(CStr::from_ptr(*items), c"faulthandler");
        pep741::PyInitConfig_FreeStrList(length, items);

        // Setting hash_seed sets use_hash_seed
        assert_eq!(initconfig_getint(config, c"use_hash_seed"), 0);
        assert_eq!(
            pep741::PyInitConfig_SetInt(config, c"hash_seed".as_ptr(), 123),
            0
        );
        assert_eq!(initconfig_getint(config, c"use_hash_seed"), 1);

        // Setting module_search_paths sets module_search_paths_set
        assert_eq!(initconfig_getint(config, c"module_search_paths_set"), 0);
        let paths: [*const c_char; 2] = [c"search".as_ptr(), c"path".as_ptr()];
        assert_eq!(
            pep741::PyInitConfig_SetStrList(
                config,
                c"module_search_paths".as_ptr(),
                paths.len(),
                paths.as_ptr()
            ),
            0
        );
        assert_eq!(initconfig_getint(config, c"module_search_paths_set"), 1);
        pep741::PyInitConfig_Free(config);
    }
    0
}

fn test_initconfig_exit() -> i32 {
    let config = pep741::PyInitConfig_Create();
    if config.is_null() {
        println!("Init allocation error");
        return 1;
    }
    unsafe {
        let argv: [*const c_char; 2] = [PROGRAM_NAME_UTF8.as_ptr(), c"--help".as_ptr()];
        assert_eq!(
            pep741::PyInitConfig_SetStrList(config, c"argv".as_ptr(), argv.len(), argv.as_ptr()),
            0
        );
        assert_eq!(
            pep741::PyInitConfig_SetInt(config, c"parse_argv".as_ptr(), 1),
            0
        );

        assert!(pep741::Py_InitializeFromInitConfig(config) < 0);

        let mut exitcode: std::os::raw::c_int = -1;
        assert_eq!(
            pep741::PyInitConfig_GetExitCode(config, &raw mut exitcode),
            1
        );
        assert_eq!(exitcode, 0);

        let mut err_msg: *const c_char = std::ptr::null();
        assert_eq!(pep741::PyInitConfig_GetError(config, &raw mut err_msg), 1);
        assert_eq!(CStr::from_ptr(err_msg), c"exit code 0");

        pep741::PyInitConfig_Free(config);
    }
    0
}

/// `my_test_extension`: a multi-phase (PEP 489) definition with the
/// `Py_mod_gil` slot, like the C original.
unsafe extern "C" fn init_my_test_extension() -> *mut weavepy::capi::object::PyObject {
    use weavepy::capi::module::{PyModuleDef, PyModuleDef_Base, PyModuleDef_Slot, PY_MOD_GIL};
    use weavepy::capi::object::PyObject;
    struct Statics {
        slots: std::cell::UnsafeCell<[PyModuleDef_Slot; 2]>,
        def: std::cell::UnsafeCell<Option<PyModuleDef>>,
    }
    // SAFETY: only touched from the initializing thread, once.
    unsafe impl Sync for Statics {}
    static STATICS: Statics = Statics {
        slots: std::cell::UnsafeCell::new([
            PyModuleDef_Slot {
                slot: PY_MOD_GIL,
                // Py_MOD_GIL_NOT_USED is the integer 1 smuggled through
                // the slot's `void *`; `dangling_mut` is the const-safe
                // way to spell that non-address.
                value: std::ptr::dangling_mut::<std::ffi::c_void>(),
            },
            PyModuleDef_Slot {
                slot: 0,
                value: std::ptr::null_mut(),
            },
        ]),
        def: std::cell::UnsafeCell::new(None),
    };
    unsafe {
        let def = STATICS.def.get();
        if (*def).is_none() {
            *def = Some(PyModuleDef {
                m_base: PyModuleDef_Base {
                    ob_base: PyObject {
                        ob_refcnt: 1,
                        ob_type: std::ptr::null_mut(),
                    },
                    m_init: None,
                    m_index: 0,
                    m_copy: std::ptr::null_mut(),
                },
                m_name: c"my_test_extension".as_ptr(),
                m_doc: std::ptr::null(),
                m_size: 0,
                m_methods: std::ptr::null_mut(),
                m_slots: STATICS.slots.get().cast::<PyModuleDef_Slot>(),
                m_traverse: std::ptr::null_mut(),
                m_clear: std::ptr::null_mut(),
                m_free: std::ptr::null_mut(),
            });
        }
        weavepy::capi::module::PyModuleDef_Init((*def).as_mut().expect("set above"))
    }
}

fn test_initconfig_module() -> i32 {
    let config = pep741::PyInitConfig_Create();
    if config.is_null() {
        println!("Init allocation error");
        return 1;
    }
    let fail = |config: *mut pep741::PyInitConfig| -> i32 {
        let mut err_msg: *const c_char = std::ptr::null();
        unsafe {
            let _ = pep741::PyInitConfig_GetError(config, &raw mut err_msg);
            let msg = if err_msg.is_null() {
                "<unknown>".to_owned()
            } else {
                CStr::from_ptr(err_msg).to_string_lossy().into_owned()
            };
            println!("Python init failed: {msg}");
        }
        1
    };
    unsafe {
        if pep741::PyInitConfig_SetStr(config, c"program_name".as_ptr(), PROGRAM_NAME_UTF8.as_ptr())
            < 0
        {
            return fail(config);
        }
        if pep741::PyInitConfig_AddModule(
            config,
            c"my_test_extension".as_ptr(),
            Some(init_my_test_extension),
        ) < 0
        {
            return fail(config);
        }
        if pep741::Py_InitializeFromInitConfig(config) < 0 {
            return fail(config);
        }
        pep741::PyInitConfig_Free(config);
    }
    if run("import my_test_extension") < 0 {
        eprintln!("unable to import my_test_extension");
        return 1;
    }
    fini();
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
