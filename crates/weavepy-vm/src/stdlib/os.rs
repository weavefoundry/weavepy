//! The `os` built-in module plus its `os.path` sub-module.
//!
//! Tracks CPython 3.13's `os` and `os.path` for the cross-platform
//! subset we need to bootstrap real scripts. The functions defer to
//! Rust's `std::env` and `std::path` so behaviour matches the host
//! OS — `os.sep` is `/` on POSIX and `\` on Windows, `os.linesep` is
//! `\n` / `\r\n` accordingly, etc.
//!
//! Anything that mutates host state (`os.chdir`, `os.environ` writes
//! propagating to spawned processes) is intentionally absent until
//! we have a clear story for sandboxing.

use crate::sync::Rc;
use crate::sync::RefCell;
use std::path::{Path, PathBuf};

use crate::error::{os_error, type_error, value_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};
use weavepy_compiler::CompareKind;

pub fn build(cache: &ModuleCache) -> Rc<PyModule> {
    // `os.path` is a *separate* module that also gets cached in
    // `sys.modules` as `"os.path"` so that `import os.path` works.
    // Eagerly install it here.
    let path_mod = build_path(cache);
    cache.insert("os.path", Object::Module(path_mod.clone()));

    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("os"),
        );
        d.insert(
            DictKey(Object::from_static("__package__")),
            Object::from_static(""),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("OS routines for the host platform."),
        );

        d.insert(
            DictKey(Object::from_static("sep")),
            Object::from_static(if cfg!(windows) { "\\" } else { "/" }),
        );
        d.insert(
            DictKey(Object::from_static("altsep")),
            if cfg!(windows) {
                Object::from_static("/")
            } else {
                Object::None
            },
        );
        d.insert(
            DictKey(Object::from_static("extsep")),
            Object::from_static("."),
        );
        d.insert(
            DictKey(Object::from_static("linesep")),
            Object::from_static(if cfg!(windows) { "\r\n" } else { "\n" }),
        );
        d.insert(
            DictKey(Object::from_static("name")),
            Object::from_static(if cfg!(windows) { "nt" } else { "posix" }),
        );
        d.insert(
            DictKey(Object::from_static("pathsep")),
            Object::from_static(if cfg!(windows) { ";" } else { ":" }),
        );
        d.insert(
            DictKey(Object::from_static("curdir")),
            Object::from_static("."),
        );
        d.insert(
            DictKey(Object::from_static("pardir")),
            Object::from_static(".."),
        );
        d.insert(
            DictKey(Object::from_static("devnull")),
            Object::from_static(if cfg!(windows) { "nul" } else { "/dev/null" }),
        );
        // CPython advertises which functions accept the `follow_symlinks`,
        // `dir_fd`, `fd`, and `effective_ids` keywords via these sets.
        // WeavePy's `os` wrappers don't implement those optional keywords,
        // so the sets are empty — callers (e.g. the verbatim `tempfile`
        // `_dont_follow_symlinks` / `_resetperms` helpers, `shutil`) then
        // take the plain-call fallback path, which is correct here.
        for name in [
            "supports_follow_symlinks",
            "supports_dir_fd",
            "supports_fd",
            "supports_effective_ids",
        ] {
            d.insert(
                DictKey(Object::from_static(name)),
                Object::new_set_from(std::iter::empty::<Object>()),
            );
        }
        // CPython sets `os.supports_bytes_environ` True on POSIX (the raw
        // environ block is bytes) and False on Windows. We model POSIX.
        d.insert(
            DictKey(Object::from_static("supports_bytes_environ")),
            Object::Bool(true),
        );
        d.insert(
            DictKey(Object::from_static("path")),
            Object::Module(path_mod),
        );
        d.insert(DictKey(Object::from_static("environ")), initial_environ());

        d.insert(
            DictKey(Object::from_static("getcwd")),
            builtin("getcwd", os_getcwd),
        );
        d.insert(
            DictKey(Object::from_static("getcwdb")),
            builtin("getcwdb", os_getcwdb),
        );
        d.insert(
            DictKey(Object::from_static("strerror")),
            builtin("strerror", os_strerror),
        );
        d.insert(
            DictKey(Object::from_static("fstat")),
            builtin("fstat", os_fstat),
        );
        // `os.stat_result` / `posix.stat_result` — the struct-sequence type
        // every `stat`/`lstat`/`fstat` result is an instance of, so tests can
        // do `isinstance(st, os.stat_result)` and `posix.stat_result`.
        d.insert(
            DictKey(Object::from_static("stat_result")),
            Object::Type(stat_result_type()),
        );
        d.insert(
            DictKey(Object::from_static("terminal_size")),
            Object::Type(terminal_size_type()),
        );
        // `os.DirEntry` — the type every `scandir` entry is an instance of.
        // `shutil`/`glob`/user code reference it for `isinstance` checks.
        d.insert(
            DictKey(Object::from_static("DirEntry")),
            Object::Type(dir_entry_type()),
        );
        // `os.defpath` — default search path for `exec*p*`/`spawn*p*`; CPython
        // hard-codes `:/bin:/usr/bin` on POSIX, `.;C:\\bin` on Windows.
        d.insert(
            DictKey(Object::from_static("defpath")),
            Object::from_static(if cfg!(windows) {
                ".;C:\\bin"
            } else {
                ":/bin:/usr/bin"
            }),
        );
        d.insert(
            DictKey(Object::from_static("getenv")),
            builtin("getenv", os_getenv),
        );
        // Low-level environ mutators. CPython's `os.putenv`/`os.unsetenv` poke
        // the C environment directly (they do *not* touch `os.environ`), which
        // is what a `preexec_fn` relies on so the value survives into the
        // exec'd child (test_subprocess.test_preexec).
        d.insert(
            DictKey(Object::from_static("putenv")),
            builtin("putenv", os_putenv),
        );
        d.insert(
            DictKey(Object::from_static("unsetenv")),
            builtin("unsetenv", os_unsetenv),
        );
        d.insert(
            DictKey(Object::from_static("getpid")),
            builtin("getpid", os_getpid),
        );
        d.insert(
            DictKey(Object::from_static("remove")),
            builtin("remove", os_remove),
        );
        d.insert(
            DictKey(Object::from_static("unlink")),
            builtin("unlink", os_remove),
        );
        d.insert(
            DictKey(Object::from_static("mkdir")),
            builtin("mkdir", os_mkdir),
        );
        d.insert(
            DictKey(Object::from_static("makedirs")),
            builtin_kw("makedirs", os_makedirs_kw),
        );
        d.insert(
            DictKey(Object::from_static("rmdir")),
            builtin("rmdir", os_rmdir),
        );
        d.insert(
            DictKey(Object::from_static("rename")),
            builtin("rename", os_rename),
        );
        d.insert(
            DictKey(Object::from_static("listdir")),
            builtin("listdir", os_listdir),
        );
        d.insert(
            DictKey(Object::from_static("urandom")),
            builtin("urandom", os_urandom),
        );
        d.insert(
            DictKey(Object::from_static("close")),
            builtin("close", os_close_stub),
        );
        d.insert(
            DictKey(Object::from_static("open")),
            builtin("open", os_open_stub),
        );
        d.insert(
            DictKey(Object::from_static("fdopen")),
            builtin_kw("fdopen", os_fdopen),
        );
        d.insert(
            DictKey(Object::from_static("stat")),
            builtin_kw("stat", os_stat_kw),
        );
        d.insert(
            DictKey(Object::from_static("lstat")),
            builtin_kw("lstat", os_lstat_kw),
        );
        d.insert(
            DictKey(Object::from_static("readlink")),
            builtin("readlink", os_readlink),
        );
        d.insert(
            DictKey(Object::from_static("chdir")),
            builtin("chdir", os_chdir),
        );
        d.insert(
            DictKey(Object::from_static("fspath")),
            builtin("fspath", os_fspath),
        );
        d.insert(
            DictKey(Object::from_static("fsdecode")),
            builtin("fsdecode", os_fsdecode),
        );
        d.insert(
            DictKey(Object::from_static("fsencode")),
            builtin("fsencode", os_fsencode),
        );
        d.insert(
            DictKey(Object::from_static("walk")),
            builtin_kw("walk", os_walk),
        );
        // Private sentinel CPython 3.13 passes as `followlinks` to make
        // `walk()` classify every symlink (and junction) as a regular file;
        // `shutil.rmtree` relies on it. Identity-compared in `os_walk`.
        d.insert(
            DictKey(Object::from_static("_walk_symlinks_as_files")),
            walk_symlinks_sentinel(),
        );
        d.insert(
            DictKey(Object::from_static("scandir")),
            builtin("scandir", os_scandir),
        );
        d.insert(
            DictKey(Object::from_static("access")),
            builtin_kw("access", os_access),
        );
        d.insert(
            DictKey(Object::from_static("pipe")),
            builtin("pipe", os_pipe),
        );
        d.insert(DictKey(Object::from_static("dup")), builtin("dup", os_dup));
        d.insert(
            DictKey(Object::from_static("dup2")),
            builtin_kw("dup2", os_dup2),
        );
        d.insert(
            DictKey(Object::from_static("lseek")),
            builtin("lseek", os_lseek),
        );
        d.insert(
            DictKey(Object::from_static("ftruncate")),
            builtin("ftruncate", os_ftruncate),
        );
        d.insert(
            DictKey(Object::from_static("get_inheritable")),
            builtin("get_inheritable", os_get_inheritable),
        );
        d.insert(
            DictKey(Object::from_static("set_inheritable")),
            builtin("set_inheritable", os_set_inheritable),
        );
        d.insert(
            DictKey(Object::from_static("isatty")),
            builtin("isatty", os_isatty),
        );
        d.insert(
            DictKey(Object::from_static("read")),
            builtin("read", os_read),
        );
        d.insert(
            DictKey(Object::from_static("write")),
            builtin("write", os_write),
        );
        d.insert(
            DictKey(Object::from_static("get_terminal_size")),
            builtin("get_terminal_size", os_get_terminal_size),
        );
        d.insert(
            DictKey(Object::from_static("cpu_count")),
            builtin("cpu_count", os_cpu_count),
        );
        d.insert(
            DictKey(Object::from_static("process_cpu_count")),
            builtin("process_cpu_count", os_cpu_count),
        );
        d.insert(
            DictKey(Object::from_static("kill")),
            builtin("kill", os_kill),
        );
        d.insert(
            DictKey(Object::from_static("waitpid")),
            builtin("waitpid", os_waitpid),
        );
        d.insert(
            DictKey(Object::from_static("waitstatus_to_exitcode")),
            builtin("waitstatus_to_exitcode", os_waitstatus_to_exitcode),
        );
        d.insert(
            DictKey(Object::from_static("set_blocking")),
            builtin("set_blocking", os_set_blocking),
        );
        d.insert(
            DictKey(Object::from_static("get_blocking")),
            builtin("get_blocking", os_get_blocking),
        );
        // Common signal numbers — match libc on POSIX.
        d.insert(DictKey(Object::from_static("SIGTERM")), Object::Int(15));
        d.insert(DictKey(Object::from_static("SIGKILL")), Object::Int(9));
        d.insert(DictKey(Object::from_static("SIGINT")), Object::Int(2));
        d.insert(DictKey(Object::from_static("SIGHUP")), Object::Int(1));
        d.insert(DictKey(Object::from_static("WNOHANG")), Object::Int(1));

        // RFC 0040 WS1: POSIX process & fd primitives (fork/exec*/
        // posix_spawn/wait*/W*/closerange/setsid/register_at_fork/…).
        crate::stdlib::os_process::register(&mut d);
        d.insert(
            DictKey(Object::from_static("get_exec_path")),
            builtin("get_exec_path", os_get_exec_path),
        );
        d.insert(
            DictKey(Object::from_static("getuid")),
            builtin("getuid", os_getuid),
        );
        d.insert(
            DictKey(Object::from_static("getgid")),
            builtin("getgid", os_getgid),
        );
        d.insert(
            DictKey(Object::from_static("geteuid")),
            builtin("geteuid", os_getuid),
        );
        d.insert(
            DictKey(Object::from_static("getegid")),
            builtin("getegid", os_getgid),
        );
        // Real-/effective-id setters. Beyond letting privilege-dropping code
        // run, their mere presence flips CPython's `skipIf(hasattr(os,
        // 'setreuid'))` guards (test_subprocess.test_user_error /
        // test_group_error), which only apply on platforms lacking them.
        #[cfg(unix)]
        {
            d.insert(
                DictKey(Object::from_static("setuid")),
                builtin("setuid", os_setuid),
            );
            d.insert(
                DictKey(Object::from_static("setgid")),
                builtin("setgid", os_setgid),
            );
            d.insert(
                DictKey(Object::from_static("seteuid")),
                builtin("seteuid", os_seteuid),
            );
            d.insert(
                DictKey(Object::from_static("setegid")),
                builtin("setegid", os_setegid),
            );
            d.insert(
                DictKey(Object::from_static("setreuid")),
                builtin("setreuid", os_setreuid),
            );
            d.insert(
                DictKey(Object::from_static("setregid")),
                builtin("setregid", os_setregid),
            );
        }
        d.insert(
            DictKey(Object::from_static("umask")),
            builtin("umask", os_umask),
        );
        d.insert(
            DictKey(Object::from_static("symlink")),
            builtin_kw("symlink", os_symlink),
        );
        d.insert(
            DictKey(Object::from_static("link")),
            builtin("link", os_link),
        );
        d.insert(
            DictKey(Object::from_static("chmod")),
            builtin_kw("chmod", os_chmod),
        );
        d.insert(
            DictKey(Object::from_static("utime")),
            builtin_kw("utime", os_utime),
        );
        d.insert(
            DictKey(Object::from_static("replace")),
            builtin("replace", os_rename),
        );
        d.insert(
            DictKey(Object::from_static("PathLike")),
            Object::Type(path_like_type()),
        );
        d.insert(DictKey(Object::from_static("O_RDONLY")), Object::Int(0));
        d.insert(DictKey(Object::from_static("O_WRONLY")), Object::Int(1));
        d.insert(DictKey(Object::from_static("O_RDWR")), Object::Int(2));
        d.insert(DictKey(Object::from_static("O_CREAT")), Object::Int(64));
        d.insert(DictKey(Object::from_static("O_EXCL")), Object::Int(128));
        d.insert(DictKey(Object::from_static("O_TRUNC")), Object::Int(512));
        d.insert(DictKey(Object::from_static("O_APPEND")), Object::Int(1024));
        d.insert(
            DictKey(Object::from_static("O_NONBLOCK")),
            Object::Int(2048),
        );
        // `lseek` whence values — identical across every POSIX platform.
        d.insert(DictKey(Object::from_static("SEEK_SET")), Object::Int(0));
        d.insert(DictKey(Object::from_static("SEEK_CUR")), Object::Int(1));
        d.insert(DictKey(Object::from_static("SEEK_END")), Object::Int(2));
        d.insert(DictKey(Object::from_static("F_OK")), Object::Int(0));
        d.insert(DictKey(Object::from_static("R_OK")), Object::Int(4));
        d.insert(DictKey(Object::from_static("W_OK")), Object::Int(2));
        d.insert(DictKey(Object::from_static("X_OK")), Object::Int(1));
        d.insert(DictKey(Object::from_static("EX_OK")), Object::Int(0));
        d.insert(DictKey(Object::from_static("EX_USAGE")), Object::Int(64));
        d.insert(DictKey(Object::from_static("EX_DATAERR")), Object::Int(65));
        d.insert(DictKey(Object::from_static("EX_NOINPUT")), Object::Int(66));
        d.insert(DictKey(Object::from_static("EX_SOFTWARE")), Object::Int(70));
        d.insert(DictKey(Object::from_static("EX_OSERR")), Object::Int(71));
        d.insert(DictKey(Object::from_static("EX_IOERR")), Object::Int(74));
    }
    Rc::new(PyModule {
        name: "os".to_owned(),
        filename: None,
        dict,
    })
}

pub fn build_path(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("os.path"),
        );
        d.insert(
            DictKey(Object::from_static("__package__")),
            Object::from_static("os"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("Operations on pathnames."),
        );
        d.insert(
            DictKey(Object::from_static("sep")),
            Object::from_static(if cfg!(windows) { "\\" } else { "/" }),
        );

        d.insert(
            DictKey(Object::from_static("join")),
            builtin("join", path_join),
        );
        d.insert(
            DictKey(Object::from_static("split")),
            builtin("split", path_split),
        );
        d.insert(
            DictKey(Object::from_static("splitext")),
            builtin("splitext", path_splitext),
        );
        d.insert(
            DictKey(Object::from_static("splitdrive")),
            builtin("splitdrive", path_splitdrive),
        );
        d.insert(
            DictKey(Object::from_static("basename")),
            builtin("basename", path_basename),
        );
        d.insert(
            DictKey(Object::from_static("dirname")),
            builtin("dirname", path_dirname),
        );
        d.insert(
            DictKey(Object::from_static("exists")),
            builtin("exists", path_exists),
        );
        d.insert(
            DictKey(Object::from_static("lexists")),
            builtin("lexists", path_lexists),
        );
        d.insert(
            DictKey(Object::from_static("isfile")),
            builtin("isfile", path_isfile),
        );
        d.insert(
            DictKey(Object::from_static("isdir")),
            builtin("isdir", path_isdir),
        );
        d.insert(
            DictKey(Object::from_static("abspath")),
            builtin("abspath", path_abspath),
        );
        d.insert(
            DictKey(Object::from_static("normpath")),
            builtin("normpath", path_normpath),
        );
        d.insert(
            DictKey(Object::from_static("normcase")),
            builtin("normcase", path_normcase),
        );
        d.insert(
            DictKey(Object::from_static("expanduser")),
            builtin("expanduser", path_expanduser),
        );
        d.insert(
            DictKey(Object::from_static("expandvars")),
            builtin("expandvars", path_expandvars),
        );
        d.insert(
            DictKey(Object::from_static("isabs")),
            builtin("isabs", path_isabs),
        );
        d.insert(
            DictKey(Object::from_static("realpath")),
            builtin("realpath", path_realpath),
        );
        d.insert(
            DictKey(Object::from_static("relpath")),
            builtin("relpath", path_relpath),
        );
        d.insert(
            DictKey(Object::from_static("commonpath")),
            builtin("commonpath", path_commonpath),
        );
        d.insert(
            DictKey(Object::from_static("commonprefix")),
            builtin("commonprefix", path_commonprefix),
        );
        d.insert(
            DictKey(Object::from_static("getsize")),
            builtin("getsize", path_getsize),
        );
        d.insert(
            DictKey(Object::from_static("getmtime")),
            builtin("getmtime", path_getmtime),
        );
        d.insert(
            DictKey(Object::from_static("getctime")),
            builtin("getctime", path_getctime),
        );
        d.insert(
            DictKey(Object::from_static("getatime")),
            builtin("getatime", path_getmtime),
        );
        d.insert(
            DictKey(Object::from_static("islink")),
            builtin("islink", path_islink),
        );
        d.insert(
            DictKey(Object::from_static("samefile")),
            builtin("samefile", path_samefile),
        );
        d.insert(
            DictKey(Object::from_static("supports_unicode_filenames")),
            Object::Bool(true),
        );
        d.insert(DictKey(Object::from_static("altsep")), Object::None);
        d.insert(
            DictKey(Object::from_static("extsep")),
            Object::from_static("."),
        );
        d.insert(
            DictKey(Object::from_static("pardir")),
            Object::from_static(".."),
        );
        d.insert(
            DictKey(Object::from_static("curdir")),
            Object::from_static("."),
        );
        d.insert(
            DictKey(Object::from_static("pathsep")),
            Object::from_static(if cfg!(windows) { ";" } else { ":" }),
        );
        d.insert(
            DictKey(Object::from_static("devnull")),
            Object::from_static(if cfg!(windows) { "nul" } else { "/dev/null" }),
        );
    }
    Rc::new(PyModule {
        name: "os.path".to_owned(),
        filename: None,
        dict,
    })
}

pub(super) fn builtin(
    name: &'static str,
    body: fn(&[Object]) -> Result<Object, RuntimeError>,
) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        binds_instance: false,
        call: Box::new(body),
        call_kw: None,
    }))
}

/// As [`builtin`], but the body also takes a keyword-argument list.
/// Use this for surfaces where CPython exposes named parameters
/// (e.g. `os.makedirs(path, mode=0o777, exist_ok=False)`).
pub(super) fn builtin_kw(
    name: &'static str,
    body: fn(&[Object], &[(String, Object)]) -> Result<Object, RuntimeError>,
) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        binds_instance: false,
        call: Box::new(move |args| body(args, &[])),
        call_kw: Some(Box::new(body)),
    }))
}

/// Extract the elements of a list/tuple/set into a `Vec<Object>`. Used by
/// the process primitives (`os_process`) to read `argv`, `file_actions`,
/// and signal sets without re-implementing the sequence protocol. Returns
/// `None` for non-sequence objects.
pub(super) fn sequence_items(o: &Object) -> Option<Vec<Object>> {
    match o {
        Object::Tuple(t) => Some(t.to_vec()),
        Object::List(l) => Some(l.borrow().clone()),
        Object::Set(s) => Some(s.borrow().iter().map(|k| k.0.clone()).collect()),
        Object::FrozenSet(s) => Some(s.iter().map(|k| k.0.clone()).collect()),
        _ => None,
    }
}

fn initial_environ() -> Object {
    let mut d = DictData::new();
    for (k, v) in std::env::vars() {
        d.insert(DictKey(Object::from_str(k)), Object::from_str(v));
    }
    Object::Dict(Rc::new(RefCell::new(d)))
}

fn os_getcwd(_args: &[Object]) -> Result<Object, RuntimeError> {
    let cwd = std::env::current_dir().map_err(|e| os_error(format!("getcwd: {e}")))?;
    Ok(Object::from_str(cwd.to_string_lossy().into_owned()))
}

/// `os.getcwdb()` — the working directory as `bytes` (the OS-encoded path).
/// `posixpath.abspath`/`realpath` call this for bytes-typed inputs.
fn os_getcwdb(_args: &[Object]) -> Result<Object, RuntimeError> {
    let cwd = std::env::current_dir().map_err(|e| os_error(format!("getcwd: {e}")))?;
    let bytes = {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            cwd.as_os_str().as_bytes().to_vec()
        }
        #[cfg(not(unix))]
        {
            cwd.to_string_lossy().into_owned().into_bytes()
        }
    };
    Ok(Object::Bytes(Rc::from(bytes.as_slice())))
}

fn os_getenv(args: &[Object]) -> Result<Object, RuntimeError> {
    let key = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Err(type_error("getenv() first arg must be str")),
    };
    let default = args.get(1).cloned().unwrap_or(Object::None);
    Ok(std::env::var_os(&key).map_or(default, |v| {
        Object::from_str(v.to_string_lossy().into_owned())
    }))
}

/// Coerce an `os.putenv`/`os.unsetenv` argument (str or bytes-like) to a
/// NUL-free C string, raising `ValueError` on an embedded NUL like CPython.
#[cfg(unix)]
fn env_cstring(o: Option<&Object>, what: &str) -> Result<std::ffi::CString, RuntimeError> {
    let bytes = match o {
        Some(Object::Str(s)) => s.as_bytes().to_vec(),
        Some(Object::Bytes(b)) => b.to_vec(),
        Some(Object::ByteArray(b)) => b.borrow().clone(),
        _ => return Err(type_error(format!("putenv() {what} must be str or bytes"))),
    };
    std::ffi::CString::new(bytes).map_err(|_| crate::error::value_error("embedded null byte"))
}

fn os_putenv(args: &[Object]) -> Result<Object, RuntimeError> {
    #[cfg(unix)]
    {
        let name = env_cstring(args.first(), "name")?;
        let value = env_cstring(args.get(1), "value")?;
        // setenv (overwrite=1) edits the live C environ, so a later `execv`
        // (which passes the inherited environ) carries the change into the
        // child — exactly what `os.putenv` promises.
        if unsafe { libc::setenv(name.as_ptr(), value.as_ptr(), 1) } != 0 {
            return Err(crate::error::io_error_to_py(&std::io::Error::last_os_error()));
        }
        Ok(Object::None)
    }
    #[cfg(not(unix))]
    {
        let _ = args;
        Err(crate::error::not_implemented_error(
            "os.putenv() is only implemented on POSIX in WeavePy",
        ))
    }
}

fn os_unsetenv(args: &[Object]) -> Result<Object, RuntimeError> {
    #[cfg(unix)]
    {
        let name = env_cstring(args.first(), "name")?;
        if unsafe { libc::unsetenv(name.as_ptr()) } != 0 {
            return Err(crate::error::io_error_to_py(&std::io::Error::last_os_error()));
        }
        Ok(Object::None)
    }
    #[cfg(not(unix))]
    {
        let _ = args;
        Err(crate::error::not_implemented_error(
            "os.unsetenv() is only implemented on POSIX in WeavePy",
        ))
    }
}

fn os_getpid(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Int(i64::from(std::process::id())))
}

fn os_remove(args: &[Object]) -> Result<Object, RuntimeError> {
    let p = first_path(args, "remove")?;
    std::fs::remove_file(&p).map_err(|e| crate::error::io_error_to_py(&e))?;
    Ok(Object::None)
}

fn os_mkdir(args: &[Object]) -> Result<Object, RuntimeError> {
    let p = first_path(args, "mkdir")?;
    // CPython: `mkdir(path, mode=0o777)`. The kernel masks `mode` with the
    // process umask, so a faithful `Path.mkdir(0o555)` ends up `0o555 & ~umask`
    // (exercised by `test_pathlib.test_mkdir_parents`).
    let mode = match args.get(1) {
        Some(m) => mode_arg(m, "mkdir")?,
        None => 0o777,
    };
    mkdir_with_mode(&p, mode)?;
    Ok(Object::None)
}

/// Extract a POSIX permission-bits argument (`int`, or an `int` subclass
/// instance) from an `os.*` mode parameter.
fn mode_arg(obj: &Object, func: &str) -> Result<u32, RuntimeError> {
    match obj.native_value().as_ref().unwrap_or(obj) {
        Object::Int(m) => Ok(*m as u32),
        Object::Bool(b) => Ok(u32::from(*b)),
        _ => Err(type_error(format!(
            "{func}: mode should be an integer, not {}",
            obj.type_name()
        ))),
    }
}

fn mkdir_with_mode(path: &str, mode: u32) -> Result<(), RuntimeError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .mode(mode)
            .create(path)
            .map_err(|e| crate::error::io_error_to_py_named(&e, Some(path)))
    }
    #[cfg(not(unix))]
    {
        let _ = mode;
        std::fs::create_dir(path).map_err(|e| crate::error::io_error_to_py_named(&e, Some(path)))
    }
}

fn os_makedirs_kw(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let p = first_path(args, "makedirs")?;
    let mut exist_ok = matches!(args.get(2), Some(Object::Bool(true)));
    for (k, v) in kwargs {
        match k.as_str() {
            "exist_ok" => {
                exist_ok = matches!(v, Object::Bool(true) | Object::Int(_));
            }
            // `mode` is accepted but ignored — Rust's `create_dir_all`
            // doesn't expose POSIX mode bits portably. Matching
            // CPython on the call surface is what matters here.
            "mode" => {}
            other => {
                return Err(crate::error::type_error(format!(
                    "makedirs() got an unexpected keyword argument '{other}'"
                )));
            }
        }
    }
    match std::fs::create_dir_all(&p) {
        Ok(()) => Ok(Object::None),
        Err(e) => {
            if exist_ok && std::path::Path::new(&p).is_dir() {
                Ok(Object::None)
            } else {
                Err(crate::error::io_error_to_py(&e))
            }
        }
    }
}

fn os_rmdir(args: &[Object]) -> Result<Object, RuntimeError> {
    let p = first_path(args, "rmdir")?;
    std::fs::remove_dir(&p).map_err(|e| crate::error::io_error_to_py(&e))?;
    Ok(Object::None)
}

fn os_rename(args: &[Object]) -> Result<Object, RuntimeError> {
    let src = first_path(args, "rename")?;
    let dst = nth_path(args, 1, "rename")?;
    std::fs::rename(&src, &dst).map_err(|e| crate::error::io_error_to_py(&e))?;
    Ok(Object::None)
}

fn os_listdir(args: &[Object]) -> Result<Object, RuntimeError> {
    // CPython: `listdir(path='.')`. `path` may be str, bytes, or any
    // `os.PathLike` (a `pathlib.Path`, which is what `Path.walk()` passes).
    // A `bytes` path yields `bytes` names; everything else yields `str`.
    let (p, want_bytes) = match args.first() {
        None | Some(Object::None) => (".".to_string(), false),
        Some(Object::Bytes(b)) => (String::from_utf8_lossy(b).into_owned(), true),
        Some(other) => (path_to_string(other, "listdir")?, false),
    };
    let mut out = Vec::new();
    let iter =
        std::fs::read_dir(&p).map_err(|e| crate::error::io_error_to_py_named(&e, Some(&p)))?;
    for entry in iter {
        let entry = entry.map_err(|e| crate::error::io_error_to_py(&e))?;
        let name = entry.file_name();
        if want_bytes {
            out.push(Object::new_bytes(
                name.to_string_lossy().into_owned().into_bytes(),
            ));
        } else {
            out.push(Object::from_str(name.to_string_lossy().into_owned()));
        }
    }
    Ok(Object::new_list(out))
}

fn os_urandom(args: &[Object]) -> Result<Object, RuntimeError> {
    let n = match args.first() {
        Some(Object::Int(n)) => *n as usize,
        _ => return Err(type_error("urandom() arg must be int")),
    };
    #[cfg(unix)]
    {
        use std::io::Read;
        let mut out = vec![0u8; n];
        if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
            if f.read_exact(&mut out).is_ok() {
                return Ok(Object::new_bytes(out));
            }
        }
        // Fallback if /dev/urandom isn't readable.
        for b in out.iter_mut() {
            *b = (std::process::id() as u8).wrapping_add(*b);
        }
        Ok(Object::new_bytes(out))
    }
    #[cfg(not(unix))]
    {
        let mut out = vec![0u8; n];
        for (i, b) in out.iter_mut().enumerate() {
            *b = ((std::process::id() as usize + i) & 0xff) as u8;
        }
        Ok(Object::new_bytes(out))
    }
}

fn os_close_stub(args: &[Object]) -> Result<Object, RuntimeError> {
    // `close(fd)` for integer fds (pipe, dup, multiprocessing). Older
    // callers also passed the string tokens we hand out from `mkstemp`;
    // those are silently accepted (closing the file in `mkstemp` is the
    // host's concern).
    match args.first() {
        Some(Object::Int(fd)) => os_close_fd(*fd),
        Some(Object::Str(_)) | None => Ok(Object::None),
        Some(other) => Err(type_error(format!(
            "close() arg must be int, got {}",
            other.type_name()
        ))),
    }
}

#[cfg(unix)]
fn os_close_fd(fd: i64) -> Result<Object, RuntimeError> {
    let rc = unsafe { libc::close(fd as i32) };
    if rc != 0 {
        return Err(crate::error::io_error_to_py(
            &std::io::Error::last_os_error(),
        ));
    }
    Ok(Object::None)
}

#[cfg(not(unix))]
fn os_close_fd(_fd: i64) -> Result<Object, RuntimeError> {
    Ok(Object::None)
}

/// `os.open(path, flags, mode=0o777)` → raw fd. The flag bits are the
/// module's own `O_*` constants (translated to `OpenOptions` here, so
/// the values never reach the host libc, whose constants may differ).
#[cfg(unix)]
fn os_open_stub(args: &[Object]) -> Result<Object, RuntimeError> {
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::io::IntoRawFd;
    let p = first_path(args, "open")?;
    let flags = args
        .get(1)
        .and_then(crate::object::Object::as_i64)
        .ok_or_else(|| crate::error::type_error("open() flags must be an int".to_owned()))?;
    // `open(path, flags, mode=0o777)` — `mode` only matters when `O_CREAT`
    // creates the file; the kernel masks it with the umask, so
    // `Path.touch(0o444)` lands `0o444 & ~umask` (test_pathlib.test_touch_mode).
    let mode = args
        .get(2)
        .and_then(crate::object::Object::as_i64)
        .unwrap_or(0o777) as u32;
    const O_WRONLY: i64 = 1;
    const O_RDWR: i64 = 2;
    const O_CREAT: i64 = 64;
    const O_EXCL: i64 = 128;
    const O_TRUNC: i64 = 512;
    const O_APPEND: i64 = 1024;
    let mut oo = std::fs::OpenOptions::new();
    match flags & 0x3 {
        O_WRONLY => oo.write(true),
        O_RDWR => oo.read(true).write(true),
        _ => oo.read(true),
    };
    if flags & O_APPEND != 0 {
        oo.append(true);
    }
    if flags & O_TRUNC != 0 {
        oo.write(true).truncate(true);
    }
    if flags & O_CREAT != 0 {
        oo.mode(mode);
        if flags & O_EXCL != 0 {
            oo.create_new(true);
        } else {
            oo.create(true);
        }
    }
    let f = oo
        .open(&p)
        .map_err(|e| crate::error::io_error_to_py_named(&e, Some(&p)))?;
    Ok(Object::Int(i64::from(f.into_raw_fd())))
}

#[cfg(not(unix))]
fn os_open_stub(_args: &[Object]) -> Result<Object, RuntimeError> {
    Err(crate::error::not_implemented_error(
        "os.open(): raw fd interface is not implemented in WeavePy yet",
    ))
}

/// `os.fdopen(fd, mode='r', ...)` — wrap an existing OS file descriptor in a
/// file object (CPython returns `io.open(fd, ...)`). WeavePy adopts the fd
/// into a `Disk`-backed `PyFile`, so `read`/`write`/`seek`/`fileno` work and
/// closing the file closes the fd.
#[cfg(unix)]
fn os_fdopen(args: &[Object], _kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    use crate::object::{FileBackend, PyFile};
    use std::os::unix::io::FromRawFd;
    let fd = args
        .first()
        .and_then(crate::object::Object::as_i64)
        .ok_or_else(|| crate::error::type_error("fdopen() fd must be an int".to_owned()))?;
    let mode = match args.get(1) {
        Some(Object::Str(s)) => s.to_string(),
        None => "r".to_owned(),
        Some(_) => {
            return Err(crate::error::type_error(
                "fdopen() mode must be str".to_owned(),
            ))
        }
    };
    // SAFETY: the caller owns `fd` (typically from `os.open`/`os.pipe`); we
    // take ownership so the resulting file's lifetime governs the descriptor.
    let file = unsafe { std::fs::File::from_raw_fd(fd as i32) };
    let pf = PyFile::new(format!("<fdopen fd={fd}>"), mode, FileBackend::Disk(file));
    pf.no_name.set(true);
    Ok(Object::File(Rc::new(pf)))
}

#[cfg(not(unix))]
fn os_fdopen(_args: &[Object], _kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    Err(crate::error::not_implemented_error(
        "os.fdopen(): raw fd interface is not implemented in WeavePy yet",
    ))
}

/// `os.stat(path, *, dir_fd=None, follow_symlinks=True)`. `follow_symlinks=False`
/// makes it an `lstat` (the link itself); `shutil.copystat`/`copy2` and
/// `pathlib`/`tempfile` pass the keyword. `dir_fd` is unsupported (only `None`).
fn os_stat_kw(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    reject_dir_fd(kwargs, "stat")?;
    // `os.stat(fd)` (an int) is `fstat`; `os.stat(path)` hits the filesystem.
    // `genericpath.exists`/`isfile`/… lean on the fd form when handed a
    // descriptor.
    if let Some(Object::Int(_) | Object::Bool(_)) = args.first() {
        return os_fstat(args);
    }
    let p = first_path(args, "stat")?;
    let meta = if dir_entry_follow(kwargs) {
        std::fs::metadata(&p)
    } else {
        std::fs::symlink_metadata(&p)
    }
    .map_err(|e| crate::error::io_error_to_py(&e))?;
    Ok(stat_result_from_meta(&meta))
}

/// `os.strerror(code)` — the OS message for an `errno`. The Rust formatter
/// appends `" (os error N)"`, which CPython's bare `strerror` does not, so
/// trim it back to just the message.
fn os_strerror(args: &[Object]) -> Result<Object, RuntimeError> {
    let code = match args.first().and_then(Object::as_i64) {
        Some(c) => c,
        None => return Err(type_error("strerror() argument must be an int")),
    };
    let full = std::io::Error::from_raw_os_error(code as i32).to_string();
    let msg = full.split(" (os error ").next().unwrap_or(&full).to_owned();
    Ok(Object::from_str(msg))
}

/// Raise the CPython "bool is used as a file descriptor" `RuntimeWarning`
/// through the live `warnings` machinery (so `assertWarns`/`catch_warnings`
/// observe it, and an escalating filter turns it into a raised error). A no-op
/// if no interpreter is published on this thread.
fn warn_bool_as_fd() -> Result<(), RuntimeError> {
    if let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() {
        // SAFETY: published by the enclosing VM frame still live on this
        // thread; the GIL keeps the pointer exclusive.
        let interp = unsafe { &mut *ptr };
        return interp.warn_runtime_from_builtin("bool is used as a file descriptor".to_owned());
    }
    Ok(())
}

/// `os.fstat(fd)` — `stat(2)` on an open descriptor. We `dup` the fd into an
/// owned `File` (so the original stays open) and read its metadata.
fn os_fstat(args: &[Object]) -> Result<Object, RuntimeError> {
    let fd = match args.first() {
        Some(Object::Bool(b)) => {
            // CPython's `PyErr_WarnEx(PyExc_RuntimeWarning, "bool is used as a
            // file descriptor", 1)` — `os.stat(True)` etc. A filter that
            // escalates the warning to an error propagates here.
            warn_bool_as_fd()?;
            i64::from(*b)
        }
        Some(Object::Int(n)) => *n,
        _ => return Err(type_error("fstat() argument must be an int")),
    };
    #[cfg(unix)]
    {
        use std::os::unix::io::FromRawFd;
        let fd = i32::try_from(fd).map_err(|_| value_error("file descriptor out of range"))?;
        // SAFETY: `dup` returns a fresh owned descriptor; wrapping it in a
        // `File` means the dup (not the caller's fd) is the one closed when
        // the temporary drops, leaving the original descriptor intact.
        let dup = unsafe { libc::dup(fd) };
        if dup < 0 {
            return Err(crate::error::io_error_to_py(
                &std::io::Error::last_os_error(),
            ));
        }
        let f = unsafe { std::fs::File::from_raw_fd(dup) };
        let meta = f.metadata().map_err(|e| crate::error::io_error_to_py(&e))?;
        Ok(stat_result_from_meta(&meta))
    }
    #[cfg(not(unix))]
    {
        let _ = fd;
        Err(crate::error::not_implemented_error(
            "os.fstat is only supported on Unix",
        ))
    }
}

/// `os.lstat(path, *, dir_fd=None)` — `stat` on the link itself. `dir_fd` is
/// unsupported (only `None`).
fn os_lstat_kw(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    reject_dir_fd(kwargs, "lstat")?;
    let p = first_path(args, "lstat")?;
    let meta = std::fs::symlink_metadata(&p).map_err(|e| crate::error::io_error_to_py(&e))?;
    Ok(stat_result_from_meta(&meta))
}

fn stat_result_from_meta(meta: &std::fs::Metadata) -> Object {
    use crate::types::PyInstance;
    let ty = stat_result_type();
    let inst = PyInstance::new(ty);
    let mut d = inst.dict.borrow_mut();
    // On Unix the OS already encodes the full `st_mode` — file-type bits
    // (S_IFREG / S_IFDIR / S_IFCHR / S_IFBLK / S_IFLNK / S_IFIFO / S_IFSOCK)
    // *and* permissions — so use it verbatim; otherwise char/block devices,
    // fifos, and sockets would all misclassify (e.g. `/dev/null` showing up
    // as a symlink). Off Unix we synthesize from the coarse `is_dir`/
    // `is_file` shape plus a best-effort permission guess.
    #[cfg(unix)]
    let mode: i64 = {
        use std::os::unix::fs::MetadataExt;
        i64::from(meta.mode())
    };
    #[cfg(not(unix))]
    let mode: i64 = {
        let kind_bits: i64 = if meta.is_dir() {
            0o040_000
        } else if meta.is_file() {
            0o100_000
        } else {
            0o120_000
        };
        let perm_bits: i64 = if meta.is_dir() {
            0o755
        } else if meta.permissions().readonly() {
            0o444
        } else {
            0o644
        };
        kind_bits | perm_bits
    };
    d.insert(
        DictKey(Object::from_static("st_size")),
        Object::Int(meta.len() as i64),
    );
    d.insert(DictKey(Object::from_static("st_mode")), Object::Int(mode));
    // On Unix derive the float `st_*time` straight from the integer
    // nanosecond fields below, so `st_atime` and `st_atime_ns` describe the
    // *same* instant (CPython invariant: `test_stat_attributes` checks they
    // agree to within tens of microseconds). Using `Metadata::accessed()` —
    // a separately-rounded `SystemTime` — drifts from the raw `atime_nsec`.
    #[cfg(unix)]
    let (atime, mtime, ctime) = {
        use std::os::unix::fs::MetadataExt;
        let ns = |s: i64, n: i64| (s as f64) + (n as f64) * 1e-9;
        (
            ns(meta.atime(), meta.atime_nsec()),
            ns(meta.mtime(), meta.mtime_nsec()),
            ns(meta.ctime(), meta.ctime_nsec()),
        )
    };
    #[cfg(not(unix))]
    let (atime, mtime, ctime) = {
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0.0_f64, |d| d.as_secs_f64());
        let atime = meta
            .accessed()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0.0_f64, |d| d.as_secs_f64());
        let ctime = meta
            .created()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(mtime, |d| d.as_secs_f64());
        (atime, mtime, ctime)
    };
    d.insert(
        DictKey(Object::from_static("st_mtime")),
        Object::Float(mtime),
    );
    d.insert(
        DictKey(Object::from_static("st_atime")),
        Object::Float(atime),
    );
    d.insert(
        DictKey(Object::from_static("st_ctime")),
        Object::Float(ctime),
    );
    // The remaining fields come straight from the OS `stat(2)` record on
    // Unix. Real `st_ino`/`st_dev` are essential: `posixpath.samefile`/
    // `samestat` compare exactly those two, so leaving them 0 made every
    // file look identical. The `_ns` integer times, `st_blocks`,
    // `st_blksize`, and `st_rdev` round out CPython's `stat_result`
    // struct-sequence (RFC 0038 WS-B).
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        d.insert(
            DictKey(Object::from_static("st_ino")),
            Object::Int(meta.ino() as i64),
        );
        d.insert(
            DictKey(Object::from_static("st_dev")),
            Object::Int(meta.dev() as i64),
        );
        d.insert(
            DictKey(Object::from_static("st_nlink")),
            Object::Int(meta.nlink() as i64),
        );
        d.insert(
            DictKey(Object::from_static("st_uid")),
            Object::Int(i64::from(meta.uid())),
        );
        d.insert(
            DictKey(Object::from_static("st_gid")),
            Object::Int(i64::from(meta.gid())),
        );
        d.insert(
            DictKey(Object::from_static("st_rdev")),
            Object::Int(meta.rdev() as i64),
        );
        d.insert(
            DictKey(Object::from_static("st_blocks")),
            Object::Int(meta.blocks() as i64),
        );
        d.insert(
            DictKey(Object::from_static("st_blksize")),
            Object::Int(meta.blksize() as i64),
        );
        d.insert(
            DictKey(Object::from_static("st_mtime_ns")),
            Object::Int(meta.mtime() * 1_000_000_000 + meta.mtime_nsec()),
        );
        d.insert(
            DictKey(Object::from_static("st_atime_ns")),
            Object::Int(meta.atime() * 1_000_000_000 + meta.atime_nsec()),
        );
        d.insert(
            DictKey(Object::from_static("st_ctime_ns")),
            Object::Int(meta.ctime() * 1_000_000_000 + meta.ctime_nsec()),
        );
    }
    #[cfg(not(unix))]
    {
        d.insert(DictKey(Object::from_static("st_ino")), Object::Int(0));
        d.insert(DictKey(Object::from_static("st_dev")), Object::Int(0));
        d.insert(DictKey(Object::from_static("st_nlink")), Object::Int(1));
        d.insert(DictKey(Object::from_static("st_uid")), Object::Int(0));
        d.insert(DictKey(Object::from_static("st_gid")), Object::Int(0));
        d.insert(DictKey(Object::from_static("st_rdev")), Object::Int(0));
        d.insert(DictKey(Object::from_static("st_blocks")), Object::Int(0));
        d.insert(
            DictKey(Object::from_static("st_blksize")),
            Object::Int(4096),
        );
        let mtime_ns = (mtime * 1e9) as i64;
        d.insert(
            DictKey(Object::from_static("st_mtime_ns")),
            Object::Int(mtime_ns),
        );
        d.insert(
            DictKey(Object::from_static("st_atime_ns")),
            Object::Int((atime * 1e9) as i64),
        );
        d.insert(
            DictKey(Object::from_static("st_ctime_ns")),
            Object::Int((ctime * 1e9) as i64),
        );
    }
    drop(d);
    Object::Instance(Rc::new(inst))
}

fn os_readlink(args: &[Object]) -> Result<Object, RuntimeError> {
    // CPython's `os.readlink` returns the same string flavour as its argument:
    // a `bytes`/bytes-`PathLike` path yields `bytes`, a `str` path yields `str`.
    let obj = args
        .first()
        .ok_or_else(|| type_error("readlink() requires a path argument"))?;
    let resolved = resolve_fspath_obj(obj, "readlink")?;
    let want_bytes = matches!(resolved, Object::Bytes(_));
    let pstr = match &resolved {
        Object::Str(s) => s.to_string(),
        Object::Bytes(b) => String::from_utf8_lossy(b).into_owned(),
        _ => unreachable!("resolve_fspath_obj returns str/bytes"),
    };
    if pstr.as_bytes().contains(&0) {
        return Err(value_error("embedded null byte"));
    }
    let t = std::fs::read_link(&pstr).map_err(|e| crate::error::io_error_to_py(&e))?;
    if want_bytes {
        use std::os::unix::ffi::OsStringExt;
        return Ok(Object::new_bytes(t.into_os_string().into_vec()));
    }
    Ok(Object::from_str(t.to_string_lossy().into_owned()))
}

/// Resolve a path argument to a concrete `str`/`bytes` object, honouring the
/// `os.PathLike` protocol once. Unlike [`path_to_string`] this preserves the
/// `bytes`-vs-`str` flavour so callers (e.g. `readlink`) can mirror it in the
/// result, matching CPython's `path_t` converter.
fn resolve_fspath_obj(obj: &Object, func: &str) -> Result<Object, RuntimeError> {
    match obj {
        Object::Str(_) | Object::Bytes(_) => Ok(obj.clone()),
        Object::ByteArray(b) => Ok(Object::new_bytes(b.borrow().clone())),
        Object::Instance(_) => {
            if let Some(n @ (Object::Str(_) | Object::Bytes(_))) = obj.native_value() {
                return Ok(n);
            }
            let ptr = crate::vm_singletons::current_interpreter_ptr().ok_or_else(|| {
                type_error(format!(
                    "{func}: path should be string, bytes or os.PathLike, not {}",
                    obj.type_name()
                ))
            })?;
            // SAFETY: published by the enclosing VM frame on this thread.
            let interp = unsafe { &mut *ptr };
            let fspath = interp.load_attr_public(obj, "__fspath__").map_err(|_| {
                type_error(format!(
                    "{func}: path should be string, bytes or os.PathLike, not {}",
                    obj.type_name()
                ))
            })?;
            match interp.call_object(fspath, &[], &[])? {
                r @ (Object::Str(_) | Object::Bytes(_)) => Ok(r),
                Object::ByteArray(b) => Ok(Object::new_bytes(b.borrow().clone())),
                other => Err(type_error(format!(
                    "expected {func}.__fspath__() to return str or bytes, not {}",
                    other.type_name()
                ))),
            }
        }
        other => Err(type_error(format!(
            "{func}: path should be string, bytes or os.PathLike, not {}",
            other.type_name()
        ))),
    }
}

fn os_chdir(args: &[Object]) -> Result<Object, RuntimeError> {
    let p = first_path(args, "chdir")?;
    // Attach the offending path so the raised OSError carries `.filename`
    // (CPython does this for path syscalls; subprocess's bad-cwd tests compare
    // `os.chdir(bad).filename` against the error surfaced from the child).
    std::env::set_current_dir(&p)
        .map_err(|e| crate::error::io_error_to_py_named(&e, Some(&p)))?;
    Ok(Object::None)
}

fn os_fspath(args: &[Object]) -> Result<Object, RuntimeError> {
    let obj = match args.first() {
        Some(o) => o,
        None => return Err(type_error("fspath() takes exactly one argument")),
    };
    match obj {
        Object::Str(_) | Object::Bytes(_) => Ok(obj.clone()),
        Object::Instance(_) => {
            // A `str`/`bytes` subclass reduces to its native value (CPython
            // `os.fspath` returns those directly).
            if let Some(n @ (Object::Str(_) | Object::Bytes(_))) = obj.native_value() {
                return Ok(n);
            }
            // Otherwise honour the `os.PathLike` protocol: call `__fspath__`
            // and require it to yield `str`/`bytes`.
            let ptr = crate::vm_singletons::current_interpreter_ptr().ok_or_else(|| {
                type_error(format!(
                    "expected str, bytes or os.PathLike object, not {}",
                    obj.type_name()
                ))
            })?;
            // SAFETY: published by the enclosing VM frame on this thread.
            let interp = unsafe { &mut *ptr };
            let fspath = interp.load_attr_public(obj, "__fspath__").map_err(|_| {
                type_error(format!(
                    "expected str, bytes or os.PathLike object, not {}",
                    obj.type_name()
                ))
            })?;
            match interp.call_object(fspath, &[], &[])? {
                r @ (Object::Str(_) | Object::Bytes(_)) => Ok(r),
                other => Err(type_error(format!(
                    "expected {}.__fspath__() to return str or bytes, not {}",
                    obj.type_name(),
                    other.type_name()
                ))),
            }
        }
        other => Err(type_error(format!(
            "expected str, bytes or os.PathLike object, not {}",
            other.type_name()
        ))),
    }
}

/// Reduce a path-like argument to a `str` or `bytes` object, mirroring
/// CPython's `os.fspath`: `str`/`bytes` pass through, an `str`/`bytes`
/// subclass instance reduces to its native value. Used by `fsdecode`/
/// `fsencode` (which themselves only special-case the str/bytes split).
fn fspath_to_str_or_bytes(obj: &Object, func: &str) -> Result<Object, RuntimeError> {
    match obj {
        Object::Str(_) | Object::Bytes(_) => Ok(obj.clone()),
        Object::Instance(_) => match obj.native_value() {
            Some(n @ (Object::Str(_) | Object::Bytes(_))) => Ok(n),
            _ => Err(type_error(format!(
                "expected str, bytes or os.PathLike object, not {}",
                obj.type_name()
            ))),
        },
        other => Err(type_error(format!(
            "{}() argument must be str, bytes, or os.PathLike object, not {}",
            func,
            other.type_name()
        ))),
    }
}

/// `os.fsdecode(filename)` — decode a `bytes` path to `str` (the filesystem
/// encoding is UTF-8 here), pass a `str` through unchanged.
fn os_fsdecode(args: &[Object]) -> Result<Object, RuntimeError> {
    let obj = args
        .first()
        .ok_or_else(|| type_error("fsdecode() takes exactly one argument (0 given)"))?;
    match fspath_to_str_or_bytes(obj, "fsdecode")? {
        s @ Object::Str(_) => Ok(s),
        Object::Bytes(b) => Ok(Object::from_str(String::from_utf8_lossy(&b).into_owned())),
        _ => unreachable!("fspath_to_str_or_bytes returns only str/bytes"),
    }
}

/// `os.fsencode(filename)` — encode a `str` path to `bytes` (UTF-8), pass a
/// `bytes` through unchanged.
fn os_fsencode(args: &[Object]) -> Result<Object, RuntimeError> {
    let obj = args
        .first()
        .ok_or_else(|| type_error("fsencode() takes exactly one argument (0 given)"))?;
    match fspath_to_str_or_bytes(obj, "fsencode")? {
        Object::Str(s) => Ok(Object::Bytes(Rc::from(s.as_bytes()))),
        b @ Object::Bytes(_) => Ok(b),
        _ => unreachable!("fspath_to_str_or_bytes returns only str/bytes"),
    }
}

/// Process-wide `os._walk_symlinks_as_files` sentinel. A bare `object()`
/// instance whose *identity* (`Rc::ptr_eq`) marks the "classify symlinks as
/// files" walk mode; memoised so the value handed back through the module
/// dict is the same object `os_walk` compares against.
fn walk_symlinks_sentinel() -> Object {
    use crate::types::PyInstance;
    static SENTINEL: std::sync::OnceLock<Object> = std::sync::OnceLock::new();
    SENTINEL
        .get_or_init(|| {
            let object_ty = crate::builtin_types::builtin_types().object_.clone();
            Object::Instance(Rc::new(PyInstance::new(object_ty)))
        })
        .clone()
}

fn os_walk(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    // `os.walk` is a lazy *generator* in CPython: callers prune the search by
    // mutating `dirnames` in place between yields, and `os.scandir` failures
    // are reported through `onerror`. Both are impossible to honour from a
    // pre-built list, so we delegate to the verbatim CPython generator vendored
    // in the frozen `_oswalk` module (which builds on our `os.scandir`).
    let ptr = crate::vm_singletons::current_interpreter_ptr()
        .ok_or_else(|| type_error("os.walk: no active interpreter"))?;
    // SAFETY: published by the enclosing VM frame on this thread.
    let interp = unsafe { &mut *ptr };
    let module = interp.import_path("_oswalk")?;
    let walk = match &module {
        Object::Module(m) => m
            .dict
            .borrow()
            .get(&DictKey(Object::from_static("walk")))
            .cloned(),
        _ => None,
    }
    .ok_or_else(|| type_error("os.walk: _oswalk.walk is unavailable"))?;
    interp.call_object(walk, args, kwargs)
}

fn os_scandir(args: &[Object]) -> Result<Object, RuntimeError> {
    // CPython's `os.scandir` accepts str, bytes, an `os.PathLike`, or no
    // argument (`.`). The *type* of the argument flows through to the
    // `DirEntry.name`/`.path` it yields — `bytes` in, `bytes` out — which the
    // verbatim `glob`/`fnmatch` bytes paths depend on.
    let (dir_path, bytes_mode) = match args.first() {
        None | Some(Object::None) => (".".to_owned(), false),
        Some(Object::Str(s)) => (s.to_string(), false),
        Some(Object::Bytes(b)) => (String::from_utf8_lossy(b).into_owned(), true),
        Some(Object::ByteArray(b)) => (String::from_utf8_lossy(&b.borrow()).into_owned(), true),
        Some(other @ Object::Instance(_)) => (path_to_string(other, "scandir")?, false),
        Some(other) => {
            return Err(type_error(format!(
                "scandir: path should be string, bytes, os.PathLike or integer, not {}",
                other.type_name()
            )))
        }
    };
    let entries: Vec<Object> = std::fs::read_dir(&dir_path)
        // CPython sets `OSError.filename` to the path that failed (e.g. a
        // `PermissionError` from `scandir` on a 0o000 dir). `shutil.rmtree`'s
        // `onexc`/`os.walk`'s `onerror` and `tempfile`'s `_resetperms` read
        // that attribute, so dropping it turns a clean error into a
        // `TypeError: ... not NoneType`.
        .map_err(|e| crate::error::io_error_to_py_named(&e, Some(&dir_path)))?
        .filter_map(|r| r.ok())
        .map(|entry| {
            let fs_path = entry.path().to_string_lossy().into_owned();
            let (name_obj, path_obj) = if bytes_mode {
                (dir_entry_name_bytes(&entry), dir_entry_path_bytes(&entry))
            } else {
                (
                    Object::from_str(entry.file_name().to_string_lossy().into_owned()),
                    Object::from_str(fs_path.clone()),
                )
            };
            build_dir_entry(name_obj, path_obj, fs_path)
        })
        .collect();
    Ok(build_scandir_iterator(entries))
}

/// `DirEntry.name` as `bytes` for a `bytes`-mode `scandir`. On Unix the OS
/// name is already a byte string (no transcoding); elsewhere we encode the
/// lossy UTF-8 form as a best effort.
fn dir_entry_name_bytes(entry: &std::fs::DirEntry) -> Object {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Object::Bytes(Rc::from(entry.file_name().as_bytes()))
    }
    #[cfg(not(unix))]
    {
        Object::Bytes(Rc::from(entry.file_name().to_string_lossy().as_bytes()))
    }
}

/// `DirEntry.path` as `bytes` for a `bytes`-mode `scandir` (see
/// [`dir_entry_name_bytes`]).
fn dir_entry_path_bytes(entry: &std::fs::DirEntry) -> Object {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Object::Bytes(Rc::from(entry.path().as_os_str().as_bytes()))
    }
    #[cfg(not(unix))]
    {
        Object::Bytes(Rc::from(entry.path().to_string_lossy().as_bytes()))
    }
}

/// `os.access(path, mode, *, dir_fd=None, effective_ids=False,
/// follow_symlinks=True)` — test real-uid/gid accessibility of *path* for
/// the `F_OK`/`R_OK`/`W_OK`/`X_OK` bitmask, defering to the platform
/// `access(2)`. Returns `False` (never raises) when the path is missing or
/// the check fails, matching CPython.
fn os_access(args: &[Object], _kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let p = first_path(args, "access")?;
    let mode = args.get(1).and_then(Object::as_i64).unwrap_or(0) as i32;
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let c = match std::ffi::CString::new(std::ffi::OsStr::new(&p).as_bytes()) {
            Ok(c) => c,
            Err(_) => return Ok(Object::Bool(false)),
        };
        let rc = unsafe { libc::access(c.as_ptr(), mode) };
        Ok(Object::Bool(rc == 0))
    }
    #[cfg(not(unix))]
    {
        // Best-effort off Unix: existence covers F_OK/R_OK; writability is
        // approximated via the read-only metadata flag; execute is assumed.
        let meta = match std::fs::metadata(&p) {
            Ok(m) => m,
            Err(_) => return Ok(Object::Bool(false)),
        };
        if mode & 2 != 0 && meta.permissions().readonly() {
            return Ok(Object::Bool(false));
        }
        Ok(Object::Bool(true))
    }
}

/// Wrap the materialised `DirEntry` list in a CPython-shaped
/// `ScandirIterator`: an iterator that is *also* a context manager
/// (`with os.scandir(p) as it:`) with a no-op `close()`. CPython's
/// `glob`/`os.walk`/`shutil` all use the `with`-statement form, which a
/// plain list can't satisfy.
fn build_scandir_iterator(entries: Vec<Object>) -> Object {
    use crate::types::{PyInstance, TypeObject};
    thread_local! {
        static CLS: RefCell<Option<Rc<TypeObject>>> = const { RefCell::new(None) };
    }
    let class = CLS.with(|slot| {
        if let Some(c) = slot.borrow().as_ref() {
            return c.clone();
        }
        let bt = crate::builtin_types::builtin_types();
        let mut dict = DictData::new();
        scandir_method(&mut dict, "__iter__", scandir_self);
        scandir_method(&mut dict, "__next__", scandir_next);
        scandir_method(&mut dict, "__enter__", scandir_self);
        scandir_method(&mut dict, "__exit__", scandir_exit);
        scandir_method(&mut dict, "close", scandir_exit);
        let cls = TypeObject::new_user("posix.ScandirIterator", vec![bt.object_.clone()], dict)
            .expect("ScandirIterator type");
        *slot.borrow_mut() = Some(cls.clone());
        cls
    });
    let it = Object::new_list(entries)
        .make_iter()
        .expect("list is always iterable");
    let inst = PyInstance::with_native(class, Object::Iter(Rc::new(RefCell::new(it))));
    Object::Instance(Rc::new(inst))
}

fn scandir_method(
    dict: &mut DictData,
    name: &'static str,
    body: fn(&[Object]) -> Result<Object, RuntimeError>,
) {
    dict.insert(
        DictKey(Object::from_static(name)),
        Object::Builtin(Rc::new(crate::object::BuiltinFn {
            name,
            binds_instance: true,
            call: Box::new(body),
            call_kw: None,
        })),
    );
}

fn scandir_self(args: &[Object]) -> Result<Object, RuntimeError> {
    args.first()
        .cloned()
        .ok_or_else(|| type_error("ScandirIterator method requires self"))
}

fn scandir_next(args: &[Object]) -> Result<Object, RuntimeError> {
    if let Some(Object::Instance(inst)) = args.first() {
        if let Some(Object::Iter(it)) = &inst.native {
            return match it.borrow_mut().next_value() {
                Some(v) => Ok(v),
                None => Err(crate::error::stop_iteration()),
            };
        }
    }
    Err(type_error(
        "ScandirIterator.__next__ requires a scandir iterator",
    ))
}

fn scandir_exit(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::None)
}

/// Whether a `DirEntry`/`stat` call should follow symlinks. CPython defaults
/// `follow_symlinks=True` for `is_dir`/`is_file`/`stat`.
fn dir_entry_follow(kwargs: &[(String, Object)]) -> bool {
    kwargs
        .iter()
        .find(|(k, _)| k == "follow_symlinks")
        .map(|(_, v)| v.is_truthy())
        .unwrap_or(true)
}

/// Build one of the lazy, `follow_symlinks`-aware `DirEntry` type predicates
/// (`is_dir`/`is_file`). CPython's `DirEntry.is_dir()` follows symlinks by
/// default (so a symlink-to-dir is a dir — the invariant the verbatim `glob`
/// uses to recurse through symlinked directories), and re-`stat`s on demand.
fn dir_entry_typecheck(name: &'static str, fs_path: String, want_dir: bool) -> Object {
    let p_pos = fs_path.clone();
    let classify = move |path: &str, follow: bool| -> bool {
        let md = if follow {
            std::fs::metadata(path)
        } else {
            std::fs::symlink_metadata(path)
        };
        md.map(|m| if want_dir { m.is_dir() } else { m.is_file() })
            .unwrap_or(false)
    };
    let classify_pos = classify;
    Object::Builtin(Rc::new(crate::object::BuiltinFn {
        name,
        binds_instance: false,
        call: Box::new(move |_args| Ok(Object::Bool(classify_pos(&p_pos, true)))),
        call_kw: Some(Box::new(move |_args, kwargs| {
            Ok(Object::Bool(classify(&fs_path, dir_entry_follow(kwargs))))
        })),
    }))
}

/// Build a CPython-compatible ``os.DirEntry`` instance: ``name``/``path``
/// attributes plus the lazy ``is_dir``/``is_file``/``is_symlink``/``stat``/
/// ``inode`` methods (all `follow_symlinks`-aware where CPython is).
/// The shared `os.DirEntry` type. CPython exposes the `DirEntry` *type* on the
/// `os` module (`os.DirEntry`), which `shutil` and user code reference for
/// `isinstance` checks; every `scandir` entry is an instance of this one type.
pub(crate) fn dir_entry_type() -> Rc<crate::types::TypeObject> {
    use crate::types::TypeObject;
    thread_local! {
        static CLS: RefCell<Option<Rc<TypeObject>>> = const { RefCell::new(None) };
    }
    CLS.with(|slot| {
        if let Some(c) = slot.borrow().as_ref() {
            return c.clone();
        }
        let bt = crate::builtin_types::builtin_types();
        let dict = DictData::new();
        let cls = TypeObject::new_user("DirEntry", vec![bt.object_.clone()], dict)
            .expect("DirEntry type");
        *slot.borrow_mut() = Some(cls.clone());
        cls
    })
}

fn build_dir_entry(name: Object, path: Object, fs_path: String) -> Object {
    use crate::object::BuiltinFn;
    use crate::types::PyInstance;
    let class = dir_entry_type();
    let inst = PyInstance::new(class);
    {
        let mut d = inst.dict.borrow_mut();
        // `name`/`path` carry the *type* of the `scandir` argument: `str`
        // entries for a `str` directory, `bytes` entries for a `bytes` one —
        // the CPython invariant the verbatim `glob` relies on for bytes globs.
        d.insert(DictKey(Object::from_static("name")), name);
        d.insert(DictKey(Object::from_static("path")), path.clone());
        // `os.PathLike`: `DirEntry.__fspath__()` returns the `.path` (str for a
        // str scandir, bytes for a bytes one). This is what lets `shutil`'s
        // `copytree` recurse with a `DirEntry` as `src` (the default
        // `copy_function is copy2` path passes the entry, not a string, to
        // `os.scandir`/`copy2`/`os.stat`).
        d.insert(
            DictKey(Object::from_static("__fspath__")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "__fspath__",
                binds_instance: false,
                call: Box::new(move |_args| Ok(path.clone())),
                call_kw: None,
            })),
        );
        d.insert(
            DictKey(Object::from_static("is_dir")),
            dir_entry_typecheck("is_dir", fs_path.clone(), true),
        );
        d.insert(
            DictKey(Object::from_static("is_file")),
            dir_entry_typecheck("is_file", fs_path.clone(), false),
        );
        // `is_symlink()` is always an lstat (no `follow_symlinks` in CPython).
        let p_sym = fs_path.clone();
        d.insert(
            DictKey(Object::from_static("is_symlink")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "is_symlink",
                binds_instance: false,
                call: Box::new(move |_args| {
                    Ok(Object::Bool(
                        std::fs::symlink_metadata(&p_sym)
                            .map(|m| m.file_type().is_symlink())
                            .unwrap_or(false),
                    ))
                }),
                call_kw: None,
            })),
        );
        // `is_junction()` — Windows reparse-point junctions; always `False`
        // on POSIX (matching `os.path.isjunction`). `os.walk`'s
        // `_walk_symlinks_as_files` mode calls this.
        d.insert(
            DictKey(Object::from_static("is_junction")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "is_junction",
                binds_instance: false,
                call: Box::new(move |_args| Ok(Object::Bool(false))),
                call_kw: None,
            })),
        );
        // `inode()` — the entry's inode number (CPython `DirEntry.inode`).
        let p_ino = fs_path.clone();
        d.insert(
            DictKey(Object::from_static("inode")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "inode",
                binds_instance: false,
                call: Box::new(move |_args| Ok(Object::Int(dir_entry_inode(&p_ino)))),
                call_kw: None,
            })),
        );
        let p_stat_pos = fs_path.clone();
        let p_stat_kw = fs_path;
        d.insert(
            DictKey(Object::from_static("stat")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "stat",
                binds_instance: false,
                call: Box::new(move |_args| dir_entry_stat(&p_stat_pos, true)),
                call_kw: Some(Box::new(move |_args, kwargs| {
                    dir_entry_stat(&p_stat_kw, dir_entry_follow(kwargs))
                })),
            })),
        );
    }
    Object::Instance(Rc::new(inst))
}

/// `DirEntry.stat(follow_symlinks=True)` — a full `stat_result` for the entry,
/// optionally on the link itself.
fn dir_entry_stat(fs_path: &str, follow: bool) -> Result<Object, RuntimeError> {
    let meta = if follow {
        std::fs::metadata(fs_path)
    } else {
        std::fs::symlink_metadata(fs_path)
    }
    .map_err(|e| crate::error::io_error_to_py(&e))?;
    Ok(stat_result_from_meta(&meta))
}

/// `DirEntry.inode()` — the entry's inode (lstat; `0` off Unix / on error).
fn dir_entry_inode(fs_path: &str) -> i64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        std::fs::symlink_metadata(fs_path)
            .map(|m| m.ino() as i64)
            .unwrap_or(0)
    }
    #[cfg(not(unix))]
    {
        let _ = fs_path;
        0
    }
}

#[cfg(unix)]
fn os_kill(args: &[Object]) -> Result<Object, RuntimeError> {
    // Accept int subclasses (e.g. `signal.Signals` enum members) for both
    // args, matching CPython's `__index__` coercion.
    let pid = match args.first().and_then(Object::as_i64) {
        Some(p) => p as libc::pid_t,
        None => return Err(type_error("kill() pid must be int")),
    };
    let sig = match args.get(1).and_then(Object::as_i64) {
        Some(s) => s as libc::c_int,
        None => return Err(type_error("kill() signal must be int")),
    };
    let rc = unsafe { libc::kill(pid, sig) };
    if rc != 0 {
        return Err(crate::error::io_error_to_py(
            &std::io::Error::last_os_error(),
        ));
    }
    Ok(Object::None)
}

#[cfg(not(unix))]
fn os_kill(_args: &[Object]) -> Result<Object, RuntimeError> {
    Err(crate::error::not_implemented_error(
        "os.kill() is only implemented on POSIX in WeavePy",
    ))
}

#[cfg(unix)]
fn os_waitpid(args: &[Object]) -> Result<Object, RuntimeError> {
    let pid = match args.first() {
        Some(Object::Int(p)) => *p as libc::pid_t,
        _ => return Err(type_error("waitpid() pid must be int")),
    };
    let options = match args.get(1) {
        Some(Object::Int(o)) => *o as libc::c_int,
        Some(Object::None) | None => 0,
        _ => return Err(type_error("waitpid() options must be int")),
    };
    let mut status: libc::c_int = 0;
    let status_ptr: *mut libc::c_int = &raw mut status;
    // Release the GIL across the (blocking, unless WNOHANG) wait so peer
    // threads run — `multiprocessing`/`subprocess` join a child on one thread
    // while result/worker handler threads keep draining pipes. Honour PEP 475
    // on `EINTR`. Mirrors `os.wait4`/`wait3`.
    let rc = loop {
        let rc =
            crate::gil::allow_threads_then(|| unsafe { libc::waitpid(pid, status_ptr, options) });
        if rc < 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EINTR) {
                service_pending_signals()?;
                continue;
            }
            return Err(crate::error::io_error_to_py(&err));
        }
        break rc;
    };
    Ok(Object::new_tuple(vec![
        Object::Int(i64::from(rc)),
        Object::Int(i64::from(status)),
    ]))
}

#[cfg(not(unix))]
fn os_waitpid(_args: &[Object]) -> Result<Object, RuntimeError> {
    Err(crate::error::not_implemented_error(
        "os.waitpid() is only implemented on POSIX in WeavePy",
    ))
}

/// `os.waitstatus_to_exitcode(status)` — convert a `wait()`/`waitpid()`
/// status to an exit code: the exit status for a normal exit, or the
/// negated signal number for a signal-terminated child. asyncio's
/// `ThreadedChildWatcher._do_waitpid` calls this from its reaper thread;
/// when it was missing the thread died with `AttributeError` and the
/// subprocess waiter future never resolved, hanging every
/// `create_subprocess_*` call (and the `test_events`/`test_subprocess`
/// suites). Mirrors CPython's `os.waitstatus_to_exitcode`.
#[cfg(unix)]
fn os_waitstatus_to_exitcode(args: &[Object]) -> Result<Object, RuntimeError> {
    let status = match args.first() {
        Some(Object::Int(s)) => *s as libc::c_int,
        Some(Object::Bool(b)) => libc::c_int::from(*b),
        _ => return Err(type_error("an integer is required")),
    };
    if libc::WIFEXITED(status) {
        Ok(Object::Int(i64::from(libc::WEXITSTATUS(status))))
    } else if libc::WIFSIGNALED(status) {
        Ok(Object::Int(i64::from(-libc::WTERMSIG(status))))
    } else if libc::WIFSTOPPED(status) {
        Err(value_error(format!(
            "process stopped by delivery of signal {}",
            libc::WSTOPSIG(status)
        )))
    } else {
        Err(value_error(format!("invalid wait status: {status}")))
    }
}

#[cfg(not(unix))]
fn os_waitstatus_to_exitcode(_args: &[Object]) -> Result<Object, RuntimeError> {
    Err(crate::error::not_implemented_error(
        "os.waitstatus_to_exitcode() is only implemented on POSIX in WeavePy",
    ))
}

/// `os.set_blocking(fd, blocking)` — toggle `O_NONBLOCK` on a file
/// descriptor via `fcntl`. asyncio's pipe and socket transports set
/// their fds non-blocking through this; without it, subprocess pipe
/// transports raised `AttributeError` mid-setup.
#[cfg(unix)]
fn os_set_blocking(args: &[Object]) -> Result<Object, RuntimeError> {
    let fd = match args.first() {
        Some(Object::Int(n)) => *n as libc::c_int,
        _ => return Err(type_error("an integer is required")),
    };
    let blocking = match args.get(1) {
        Some(Object::Bool(b)) => *b,
        Some(Object::Int(n)) => *n != 0,
        _ => return Err(type_error("set_blocking() takes a bool")),
    };
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(crate::error::io_error_to_py(
            &std::io::Error::last_os_error(),
        ));
    }
    let new = if blocking {
        flags & !libc::O_NONBLOCK
    } else {
        flags | libc::O_NONBLOCK
    };
    if unsafe { libc::fcntl(fd, libc::F_SETFL, new) } < 0 {
        return Err(crate::error::io_error_to_py(
            &std::io::Error::last_os_error(),
        ));
    }
    Ok(Object::None)
}

#[cfg(not(unix))]
fn os_set_blocking(_args: &[Object]) -> Result<Object, RuntimeError> {
    Err(crate::error::not_implemented_error(
        "os.set_blocking() is only implemented on POSIX in WeavePy",
    ))
}

/// `os.get_blocking(fd)` — `True` if `fd` is in blocking mode (i.e.
/// `O_NONBLOCK` is clear).
#[cfg(unix)]
fn os_get_blocking(args: &[Object]) -> Result<Object, RuntimeError> {
    let fd = match args.first() {
        Some(Object::Int(n)) => *n as libc::c_int,
        _ => return Err(type_error("an integer is required")),
    };
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(crate::error::io_error_to_py(
            &std::io::Error::last_os_error(),
        ));
    }
    Ok(Object::Bool(flags & libc::O_NONBLOCK == 0))
}

#[cfg(not(unix))]
fn os_get_blocking(_args: &[Object]) -> Result<Object, RuntimeError> {
    Err(crate::error::not_implemented_error(
        "os.get_blocking() is only implemented on POSIX in WeavePy",
    ))
}

fn os_pipe(_args: &[Object]) -> Result<Object, RuntimeError> {
    #[cfg(unix)]
    {
        let mut fds = [0i32; 2];
        // PEP 446: descriptors created by Python are non-inheritable
        // (close-on-exec). This is also load-bearing for
        // `_posixsubprocess.fork_exec`'s error pipe: the write end must
        // auto-close on a successful `exec` so the parent reads EOF and
        // knows the child launched. Use `pipe2(O_CLOEXEC)` where it exists
        // (atomic), else `pipe()` + `fcntl(FD_CLOEXEC)`.
        #[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "netbsd"))]
        let rc = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };
        #[cfg(not(any(target_os = "linux", target_os = "freebsd", target_os = "netbsd")))]
        let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
        if rc != 0 {
            return Err(crate::error::os_error("pipe() failed"));
        }
        #[cfg(not(any(target_os = "linux", target_os = "freebsd", target_os = "netbsd")))]
        unsafe {
            for &fd in &fds {
                let flags = libc::fcntl(fd, libc::F_GETFD);
                if flags >= 0 {
                    libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
                }
            }
        }
        Ok(Object::new_tuple(vec![
            Object::Int(i64::from(fds[0])),
            Object::Int(i64::from(fds[1])),
        ]))
    }
    #[cfg(not(unix))]
    {
        Err(crate::error::not_implemented_error(
            "os.pipe() is only implemented on POSIX in WeavePy",
        ))
    }
}

fn os_dup(args: &[Object]) -> Result<Object, RuntimeError> {
    let fd = match args.first() {
        Some(Object::Int(i)) => *i as i32,
        _ => return Err(type_error("dup() arg must be int")),
    };
    #[cfg(unix)]
    {
        let new = unsafe { libc::dup(fd) };
        if new < 0 {
            return Err(crate::error::os_error("dup() failed"));
        }
        Ok(Object::Int(i64::from(new)))
    }
    #[cfg(not(unix))]
    {
        let _ = fd;
        Err(crate::error::not_implemented_error(
            "os.dup() is only implemented on POSIX in WeavePy",
        ))
    }
}

fn os_dup2(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let fd = match args.first() {
        Some(Object::Int(i)) => *i as i32,
        _ => return Err(type_error("dup2() arg must be int")),
    };
    let newfd = match args.get(1) {
        Some(Object::Int(i)) => *i as i32,
        _ => return Err(type_error("dup2() arg2 must be int")),
    };
    // CPython's `os.dup2(fd, fd2, inheritable=True)` — `dup2` itself produces
    // an inheritable (CLOEXEC-clear) descriptor, so we only have to *set*
    // close-on-exec afterward when the caller asks for a non-inheritable copy.
    let inheritable = match args
        .get(2)
        .or_else(|| kwargs.iter().find(|(k, _)| k == "inheritable").map(|(_, v)| v))
    {
        None => true,
        Some(Object::Bool(b)) => *b,
        Some(Object::Int(n)) => *n != 0,
        Some(_) => return Err(type_error("dup2() inheritable must be bool")),
    };
    #[cfg(unix)]
    {
        let new = unsafe { libc::dup2(fd, newfd) };
        if new < 0 {
            return Err(crate::error::io_error_to_py(
                &std::io::Error::last_os_error(),
            ));
        }
        if !inheritable {
            let flags = unsafe { libc::fcntl(new, libc::F_GETFD) };
            if flags >= 0 {
                unsafe { libc::fcntl(new, libc::F_SETFD, flags | libc::FD_CLOEXEC) };
            }
        }
        Ok(Object::Int(i64::from(new)))
    }
    #[cfg(not(unix))]
    {
        let _ = (fd, newfd, inheritable);
        Err(crate::error::not_implemented_error(
            "os.dup2() is only implemented on POSIX in WeavePy",
        ))
    }
}

/// `os.lseek(fd, pos, how)` — reposition the kernel file offset and return
/// the new absolute offset. `how` is one of `SEEK_SET`/`SEEK_CUR`/`SEEK_END`.
fn os_lseek(args: &[Object]) -> Result<Object, RuntimeError> {
    let fd = match args.first() {
        Some(Object::Int(i)) => *i as i32,
        _ => return Err(type_error("lseek() fd must be int")),
    };
    let pos = match args.get(1) {
        Some(Object::Int(i)) => *i,
        Some(Object::Bool(b)) => i64::from(*b),
        _ => return Err(type_error("lseek() pos must be int")),
    };
    let how = match args.get(2) {
        Some(Object::Int(i)) => *i as i32,
        _ => return Err(type_error("lseek() how must be int")),
    };
    #[cfg(unix)]
    {
        let off = unsafe { libc::lseek(fd, pos as libc::off_t, how) };
        if off < 0 {
            return Err(crate::error::io_error_to_py(
                &std::io::Error::last_os_error(),
            ));
        }
        Ok(Object::Int(off as i64))
    }
    #[cfg(not(unix))]
    {
        let _ = (fd, pos, how);
        Err(crate::error::not_implemented_error(
            "os.lseek() is only implemented on POSIX in WeavePy",
        ))
    }
}

/// `os.ftruncate(fd, length)` — truncate (or extend) the file behind an
/// open descriptor to `length` bytes. Backs `io.FileIO.truncate()` and the
/// buffered `truncate()` path `test_io` exercises.
fn os_ftruncate(args: &[Object]) -> Result<Object, RuntimeError> {
    let fd = match args.first() {
        Some(Object::Int(i)) => *i as i32,
        Some(Object::Bool(b)) => i32::from(*b),
        _ => return Err(type_error("ftruncate() fd must be int")),
    };
    let length = match args.get(1) {
        Some(Object::Int(i)) => *i,
        Some(Object::Bool(b)) => i64::from(*b),
        _ => return Err(type_error("ftruncate() length must be int")),
    };
    if length < 0 {
        return Err(crate::error::io_error_to_py(&std::io::Error::from_raw_os_error(
            22, // EINVAL
        )));
    }
    #[cfg(unix)]
    {
        let rc = unsafe { libc::ftruncate(fd, length as libc::off_t) };
        if rc != 0 {
            return Err(crate::error::io_error_to_py(
                &std::io::Error::last_os_error(),
            ));
        }
        Ok(Object::None)
    }
    #[cfg(not(unix))]
    {
        let _ = (fd, length);
        Err(crate::error::not_implemented_error(
            "os.ftruncate() is only implemented on POSIX in WeavePy",
        ))
    }
}

/// `os.get_inheritable(fd)` — a descriptor is inheritable iff its
/// close-on-exec flag is clear (CPython's `_Py_get_inheritable`).
fn os_get_inheritable(args: &[Object]) -> Result<Object, RuntimeError> {
    let fd = match args.first() {
        Some(Object::Int(i)) => *i as i32,
        Some(Object::Bool(b)) => i32::from(*b),
        _ => return Err(type_error("get_inheritable() arg must be int")),
    };
    #[cfg(unix)]
    {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        if flags < 0 {
            return Err(crate::error::io_error_to_py(
                &std::io::Error::last_os_error(),
            ));
        }
        Ok(Object::Bool(flags & libc::FD_CLOEXEC == 0))
    }
    #[cfg(not(unix))]
    {
        let _ = fd;
        Err(crate::error::not_implemented_error(
            "os.get_inheritable() is only implemented on POSIX in WeavePy",
        ))
    }
}

/// `os.set_inheritable(fd, inheritable)` — toggle the close-on-exec flag
/// (CPython's `_Py_set_inheritable`).
fn os_set_inheritable(args: &[Object]) -> Result<Object, RuntimeError> {
    let fd = match args.first() {
        Some(Object::Int(i)) => *i as i32,
        Some(Object::Bool(b)) => i32::from(*b),
        _ => return Err(type_error("set_inheritable() arg must be int")),
    };
    let inheritable = match args.get(1) {
        Some(Object::Bool(b)) => *b,
        Some(Object::Int(n)) => *n != 0,
        _ => return Err(type_error("set_inheritable() arg2 must be int")),
    };
    #[cfg(unix)]
    {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        if flags < 0 {
            return Err(crate::error::io_error_to_py(
                &std::io::Error::last_os_error(),
            ));
        }
        let new = if inheritable {
            flags & !libc::FD_CLOEXEC
        } else {
            flags | libc::FD_CLOEXEC
        };
        if new != flags && unsafe { libc::fcntl(fd, libc::F_SETFD, new) } < 0 {
            return Err(crate::error::io_error_to_py(
                &std::io::Error::last_os_error(),
            ));
        }
        Ok(Object::None)
    }
    #[cfg(not(unix))]
    {
        let _ = (fd, inheritable);
        Err(crate::error::not_implemented_error(
            "os.set_inheritable() is only implemented on POSIX in WeavePy",
        ))
    }
}

fn os_isatty(args: &[Object]) -> Result<Object, RuntimeError> {
    let fd = match args.first() {
        Some(Object::Int(i)) => *i,
        _ => return Err(type_error("isatty() arg must be int")),
    };
    #[cfg(unix)]
    {
        let r = unsafe { libc::isatty(fd as i32) };
        Ok(Object::Bool(r != 0))
    }
    #[cfg(not(unix))]
    {
        let _ = fd;
        Ok(Object::Bool(false))
    }
}

fn os_read(args: &[Object]) -> Result<Object, RuntimeError> {
    let fd = match args.first() {
        Some(Object::Int(i)) => *i as i32,
        _ => return Err(type_error("read() arg must be int")),
    };
    let n = match args.get(1) {
        Some(Object::Int(n)) => *n as usize,
        _ => return Err(type_error("read() arg2 must be int")),
    };
    #[cfg(unix)]
    {
        let mut buf = vec![0u8; n];
        let ptr = buf.as_mut_ptr();
        // Release the GIL across the (possibly blocking) read so peer threads
        // run. Without this a single blocking `os.read` — e.g. a
        // `multiprocessing.Pool` result-handler thread parked on its outqueue
        // pipe, or any `Connection.recv` (POSIX `Connection._read = os.read`) —
        // holds the GIL for its whole wait and deadlocks every other thread in
        // the interpreter (the task-handler can never deliver work). Mirrors
        // `os_write` and CPython's `Py_BEGIN_ALLOW_THREADS` around `read(2)`.
        // Honour PEP 475: on `EINTR` run the tripped Python signal handler
        // before retrying (a handler that raises abandons the read).
        loop {
            let r =
                crate::gil::allow_threads_then(|| unsafe { libc::read(fd, ptr.cast(), n) });
            if r < 0 {
                // Carry errno so callers see the right subclass — e.g.
                // `BlockingIOError` (EAGAIN) on a non-blocking fd and
                // `BrokenPipeError` (EPIPE). `subprocess.communicate` relies on
                // this when draining pipes through a selector loop.
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() == Some(libc::EINTR) {
                    service_pending_signals()?;
                    continue;
                }
                return Err(crate::error::io_error_to_py(&err));
            }
            buf.truncate(r as usize);
            return Ok(Object::new_bytes(buf));
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (fd, n);
        Err(crate::error::not_implemented_error(
            "os.read() is only implemented on POSIX in WeavePy",
        ))
    }
}

/// Run any tripped OS-signal handlers on the main thread, propagating a
/// handler that raises (PEP 475). A no-op off the main thread (Python
/// signal handlers only run there) and when nothing is pending. Used by the
/// blocking `os` syscalls so an `EINTR` runs the handler before retrying.
#[cfg(unix)]
fn service_pending_signals() -> Result<(), RuntimeError> {
    if !crate::gil::is_main_thread() || !crate::stdlib::signal_mod::signals_pending() {
        return Ok(());
    }
    if let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() {
        // SAFETY: published by the active builtin call on this (main) thread;
        // the interpreter outlives this synchronous re-entrant call, mirroring
        // the `select`/`_thread` blocking-signal pattern.
        let interp = unsafe { &mut *ptr };
        interp.run_pending_signals_public()?;
    }
    Ok(())
}

fn os_write(args: &[Object]) -> Result<Object, RuntimeError> {
    let fd = match args.first() {
        Some(Object::Int(i)) => *i as i32,
        _ => return Err(type_error("write() arg must be int")),
    };
    let data = match args.get(1) {
        Some(Object::Bytes(b)) => b.to_vec(),
        Some(Object::ByteArray(b)) => b.borrow().clone(),
        // `subprocess.communicate(memoryview(...))` slices its input buffer
        // and hands the resulting memoryview straight to `os.write`; CPython
        // accepts any buffer-protocol object here, so materialise the view.
        Some(Object::MemoryView(mv)) => mv.to_bytes(),
        Some(Object::Str(s)) => s.as_bytes().to_vec(),
        _ => return Err(type_error("write() arg2 must be bytes-like")),
    };
    #[cfg(unix)]
    {
        // Release the GIL across the (possibly blocking) write so peers run,
        // and honour PEP 475: when a signal interrupts the write (`EINTR`),
        // run the tripped Python handler before retrying. A handler that
        // raises (e.g. a `SIGALRM` that does `1/0`) then abandons a write
        // blocked on a full pipe instead of looping forever — exactly what
        // `test_io`'s `SignalsTest.test_interrupted_write_*` exercises.
        loop {
            let r = crate::gil::allow_threads_then(|| unsafe {
                libc::write(fd, data.as_ptr().cast(), data.len())
            });
            if r < 0 {
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() == Some(libc::EINTR) {
                    service_pending_signals()?;
                    continue;
                }
                return Err(crate::error::io_error_to_py(&err));
            }
            return Ok(Object::Int(r as i64));
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (fd, data);
        Err(crate::error::not_implemented_error(
            "os.write() is only implemented on POSIX in WeavePy",
        ))
    }
}

/// `os.get_terminal_size(fd=STDOUT_FILENO)` — query the controlling tty's
/// window size via `TIOCGWINSZ`, returning an `os.terminal_size`. CPython
/// raises `OSError` when `fd` is not a tty (e.g. output redirected to a pipe,
/// as under the conformance harness); verbatim `shutil.get_terminal_size`
/// catches that and substitutes its fallback, so faithfully raising here is
/// load-bearing rather than returning a bogus 80x24.
fn os_get_terminal_size(args: &[Object]) -> Result<Object, RuntimeError> {
    #[cfg(unix)]
    {
        let fd = match args.first() {
            Some(Object::Int(n)) => *n as i32,
            Some(Object::Bool(b)) => i32::from(*b),
            None | Some(Object::None) => 1, // STDOUT_FILENO
            _ => return Err(type_error("an integer is required")),
        };
        let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) };
        if rc != 0 {
            return Err(crate::error::io_error_to_py(
                &std::io::Error::last_os_error(),
            ));
        }
        if ws.ws_col == 0 {
            return Err(crate::error::io_error_to_py(
                &std::io::Error::from_raw_os_error(libc::ENOTTY),
            ));
        }
        Ok(make_terminal_size(
            i64::from(ws.ws_col),
            i64::from(ws.ws_row),
        ))
    }
    #[cfg(not(unix))]
    {
        let _ = args;
        Ok(make_terminal_size(80, 24))
    }
}

fn os_cpu_count(_args: &[Object]) -> Result<Object, RuntimeError> {
    let n = std::thread::available_parallelism()
        .map(|n| n.get() as i64)
        .unwrap_or(1);
    Ok(Object::Int(n))
}

fn os_get_exec_path(_args: &[Object]) -> Result<Object, RuntimeError> {
    let sep = if cfg!(windows) { ';' } else { ':' };
    let path = std::env::var("PATH").unwrap_or_default();
    let parts: Vec<Object> = path
        .split(sep)
        .map(|s| Object::from_str(s.to_owned()))
        .collect();
    Ok(Object::new_list(parts))
}

fn os_getuid(_args: &[Object]) -> Result<Object, RuntimeError> {
    #[cfg(unix)]
    {
        Ok(Object::Int(i64::from(unsafe { libc::getuid() })))
    }
    #[cfg(not(unix))]
    {
        Ok(Object::Int(0))
    }
}

fn os_getgid(_args: &[Object]) -> Result<Object, RuntimeError> {
    #[cfg(unix)]
    {
        Ok(Object::Int(i64::from(unsafe { libc::getgid() })))
    }
    #[cfg(not(unix))]
    {
        Ok(Object::Int(0))
    }
}

/// Shared id-converter for the `set*id` family. CPython routes these through
/// `_Py_Uid_Converter`/`_Py_Gid_Converter`, which reject anything outside the
/// platform's unsigned 32-bit id range with `OverflowError`/`ValueError`.
#[cfg(unix)]
fn id_arg(args: &[Object], idx: usize, what: &str) -> Result<u32, RuntimeError> {
    // Mirror CPython's `_Py_Uid_Converter`/`_Py_Gid_Converter`:
    //  * a non-integer argument is a `TypeError`,
    //  * the sentinel `-1` is accepted and forwarded as `(id_t)-1`,
    //  * any other value outside the unsigned 32-bit id range is an
    //    `OverflowError` (not a `ValueError`).
    let value = match args.get(idx) {
        Some(Object::Bool(b)) => i64::from(*b),
        Some(Object::Int(i)) => *i,
        Some(other) => other
            .as_i64()
            .ok_or_else(|| type_error(format!("{what} should be integer, not {}", other.type_name())))?,
        None => return Err(type_error(format!("{what} should be integer"))),
    };
    if value == -1 {
        return Ok(u32::MAX);
    }
    if value < 0 || value > i64::from(u32::MAX) {
        return Err(crate::error::overflow_error(format!("{what} is not in range")));
    }
    Ok(value as u32)
}

#[cfg(unix)]
fn os_setuid(args: &[Object]) -> Result<Object, RuntimeError> {
    let uid = id_arg(args, 0, "uid")?;
    if unsafe { libc::setuid(uid as libc::uid_t) } != 0 {
        return Err(crate::error::io_error_to_py(&std::io::Error::last_os_error()));
    }
    Ok(Object::None)
}

#[cfg(unix)]
fn os_setgid(args: &[Object]) -> Result<Object, RuntimeError> {
    let gid = id_arg(args, 0, "gid")?;
    if unsafe { libc::setgid(gid as libc::gid_t) } != 0 {
        return Err(crate::error::io_error_to_py(&std::io::Error::last_os_error()));
    }
    Ok(Object::None)
}

#[cfg(unix)]
fn os_seteuid(args: &[Object]) -> Result<Object, RuntimeError> {
    let uid = id_arg(args, 0, "uid")?;
    if unsafe { libc::seteuid(uid as libc::uid_t) } != 0 {
        return Err(crate::error::io_error_to_py(&std::io::Error::last_os_error()));
    }
    Ok(Object::None)
}

#[cfg(unix)]
fn os_setegid(args: &[Object]) -> Result<Object, RuntimeError> {
    let gid = id_arg(args, 0, "gid")?;
    if unsafe { libc::setegid(gid as libc::gid_t) } != 0 {
        return Err(crate::error::io_error_to_py(&std::io::Error::last_os_error()));
    }
    Ok(Object::None)
}

/// Like [`id_arg`] but accepts the special value `-1`, which `setre*id` use to
/// mean "leave this id unchanged"; it is forwarded as `(id_t)-1`.
#[cfg(unix)]
fn id_arg_or_keep(args: &[Object], idx: usize, what: &str) -> Result<libc::uid_t, RuntimeError> {
    match args.get(idx) {
        Some(Object::Int(-1)) => Ok((-1i32) as libc::uid_t),
        _ => id_arg(args, idx, what).map(|v| v as libc::uid_t),
    }
}

#[cfg(unix)]
fn os_setreuid(args: &[Object]) -> Result<Object, RuntimeError> {
    let ruid = id_arg_or_keep(args, 0, "ruid")?;
    let euid = id_arg_or_keep(args, 1, "euid")?;
    if unsafe { libc::setreuid(ruid, euid) } != 0 {
        return Err(crate::error::io_error_to_py(&std::io::Error::last_os_error()));
    }
    Ok(Object::None)
}

#[cfg(unix)]
fn os_setregid(args: &[Object]) -> Result<Object, RuntimeError> {
    let rgid = id_arg_or_keep(args, 0, "rgid")? as libc::gid_t;
    let egid = id_arg_or_keep(args, 1, "egid")? as libc::gid_t;
    if unsafe { libc::setregid(rgid, egid) } != 0 {
        return Err(crate::error::io_error_to_py(&std::io::Error::last_os_error()));
    }
    Ok(Object::None)
}

fn os_umask(args: &[Object]) -> Result<Object, RuntimeError> {
    let mask = match args.first() {
        Some(Object::Int(i)) => *i as u32,
        _ => return Err(type_error("umask() arg must be int")),
    };
    #[cfg(unix)]
    {
        let old = unsafe { libc::umask(mask as libc::mode_t) };
        Ok(Object::Int(i64::from(old)))
    }
    #[cfg(not(unix))]
    {
        let _ = mask;
        Ok(Object::Int(0))
    }
}

fn os_symlink(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let src = first_path(args, "symlink")?;
    // `dst` is positional index 1, or the `dst`/`target`-less form; CPython's
    // signature is `symlink(src, dst, target_is_directory=False, *, dir_fd=None)`.
    // Both ends accept `os.PathLike` (`pathlib.Path`), str, or bytes.
    let dst = nth_path(args, 1, "symlink")?;
    // `dir_fd` (keyword-only) is unsupported; reject a non-`None` value rather
    // than silently ignoring it. `target_is_directory` is a Windows-only hint
    // and is accepted-and-ignored on POSIX, exactly like CPython.
    if let Some((_, v)) = kwargs.iter().find(|(k, _)| k == "dir_fd") {
        if !matches!(v, Object::None) {
            return Err(crate::error::not_implemented_error(
                "os.symlink() dir_fd is not supported in WeavePy",
            ));
        }
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&src, &dst).map_err(|e| crate::error::io_error_to_py(&e))?;
        Ok(Object::None)
    }
    #[cfg(not(unix))]
    {
        let _ = (src, dst);
        Err(crate::error::not_implemented_error(
            "os.symlink() is only implemented on POSIX in WeavePy",
        ))
    }
}

fn os_link(args: &[Object]) -> Result<Object, RuntimeError> {
    let src = first_path(args, "link")?;
    let dst = nth_path(args, 1, "link")?;
    std::fs::hard_link(&src, &dst).map_err(|e| crate::error::io_error_to_py(&e))?;
    Ok(Object::None)
}

/// `os.chmod(path, mode, *, dir_fd=None, follow_symlinks=True)`. `shutil`'s
/// `copymode`/`copystat` and `_copytree` pass `follow_symlinks=`; on a symlink
/// with `follow_symlinks=False` we chmod the link via `fchmodat` where the
/// platform supports it, else fall back to the target (matching CPython's
/// best-effort `lchmod` behaviour on Linux).
fn os_chmod(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    reject_dir_fd(kwargs, "chmod")?;
    let p = first_path(args, "chmod")?;
    let mode = match args.get(1) {
        Some(Object::Int(m)) => *m as u32,
        _ => return Err(type_error("chmod() mode must be int")),
    };
    let follow = dir_entry_follow(kwargs);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if !follow {
            // chmod the link itself, not its target.
            let cpath = std::ffi::CString::new(p.as_bytes())
                .map_err(|_| value_error("embedded null character in path"))?;
            // SAFETY: `cpath` outlives the call.
            let rc = unsafe {
                libc::fchmodat(
                    libc::AT_FDCWD,
                    cpath.as_ptr(),
                    mode as libc::mode_t,
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            };
            if rc != 0 {
                return Err(crate::error::io_error_to_py(
                    &std::io::Error::last_os_error(),
                ));
            }
            return Ok(Object::None);
        }
        let mut perms = std::fs::metadata(&p)
            .map_err(|e| crate::error::io_error_to_py(&e))?
            .permissions();
        perms.set_mode(mode);
        std::fs::set_permissions(&p, perms).map_err(|e| crate::error::io_error_to_py(&e))?;
        Ok(Object::None)
    }
    #[cfg(not(unix))]
    {
        let _ = (p, mode, follow);
        Ok(Object::None)
    }
}

/// `os.utime(path, times=None, *, ns=None, dir_fd=None, follow_symlinks=True)`.
/// Sets the access/modification times via `utimensat(2)`. `times` is a
/// `(atime, mtime)` float-seconds pair; `ns` an integer-nanoseconds pair;
/// neither → "now". `shutil.copystat` drives the `ns=` path.
fn os_utime(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    reject_dir_fd(kwargs, "utime")?;
    let p = first_path(args, "utime")?;
    let kw = |name: &str| {
        kwargs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
            .filter(|o| !matches!(o, Object::None))
    };
    let times = args.get(1).cloned().filter(|o| !matches!(o, Object::None));
    let ns = kw("ns");
    if times.is_some() && ns.is_some() {
        return Err(value_error(
            "utime: you may specify either 'times' or 'ns' but not both",
        ));
    }
    #[cfg(unix)]
    {
        let (atspec, mtspec) = if let Some(ns_obj) = ns {
            let (a, m) = utime_pair_int(&ns_obj, "ns")?;
            (ns_to_timespec(a), ns_to_timespec(m))
        } else if let Some(t_obj) = times {
            let (a, m) = utime_pair_float(&t_obj, "times")?;
            (secs_to_timespec(a), secs_to_timespec(m))
        } else {
            let now = libc::timespec {
                tv_sec: 0,
                tv_nsec: libc::UTIME_NOW,
            };
            (now, now)
        };
        let flags = if dir_entry_follow(kwargs) {
            0
        } else {
            libc::AT_SYMLINK_NOFOLLOW
        };
        let cpath = std::ffi::CString::new(p.as_bytes())
            .map_err(|_| value_error("embedded null character in path"))?;
        let specs = [atspec, mtspec];
        // SAFETY: `cpath` and `specs` outlive the call; `utimensat` only reads them.
        let rc = unsafe { libc::utimensat(libc::AT_FDCWD, cpath.as_ptr(), specs.as_ptr(), flags) };
        if rc != 0 {
            return Err(crate::error::io_error_to_py(
                &std::io::Error::last_os_error(),
            ));
        }
        Ok(Object::None)
    }
    #[cfg(not(unix))]
    {
        let _ = (times, ns);
        std::fs::metadata(&p).map_err(|e| crate::error::io_error_to_py(&e))?;
        Ok(Object::None)
    }
}

/// Reject an unsupported non-`None` `dir_fd=` keyword the way CPython rejects
/// it on platforms lacking the capability (`NotImplementedError`).
fn reject_dir_fd(kwargs: &[(String, Object)], func: &str) -> Result<(), RuntimeError> {
    if let Some((_, v)) = kwargs.iter().find(|(k, _)| k == "dir_fd") {
        if !matches!(v, Object::None) {
            return Err(crate::error::not_implemented_error(format!(
                "{func}: dir_fd unavailable on this platform"
            )));
        }
    }
    Ok(())
}

/// Split a 2-element `(atime, mtime)` int/tuple-or-list into a pair of i64
/// nanoseconds for `os.utime(ns=…)`.
#[cfg(unix)]
fn utime_pair_int(o: &Object, name: &str) -> Result<(i64, i64), RuntimeError> {
    let (a, b) = utime_pair(o, name)?;
    let to_i = |x: &Object| {
        x.as_i64()
            .ok_or_else(|| type_error(format!("utime: '{name}' must be a tuple of two ints")))
    };
    Ok((to_i(&a)?, to_i(&b)?))
}

/// Split a 2-element `(atime, mtime)` float-seconds tuple-or-list for
/// `os.utime(times=…)`.
#[cfg(unix)]
fn utime_pair_float(o: &Object, name: &str) -> Result<(f64, f64), RuntimeError> {
    let (a, b) = utime_pair(o, name)?;
    let to_f = |x: &Object| {
        crate::builtins::coerce_f64_opt(x)
            .ok()
            .flatten()
            .ok_or_else(|| type_error(format!("utime: '{name}' must be a tuple of two floats")))
    };
    Ok((to_f(&a)?, to_f(&b)?))
}

#[cfg(unix)]
fn utime_pair(o: &Object, name: &str) -> Result<(Object, Object), RuntimeError> {
    match o {
        Object::Tuple(t) if t.len() == 2 => Ok((t[0].clone(), t[1].clone())),
        Object::List(l) if l.borrow().len() == 2 => {
            let b = l.borrow();
            Ok((b[0].clone(), b[1].clone()))
        }
        _ => Err(type_error(format!(
            "utime: '{name}' must be a tuple of two items"
        ))),
    }
}

#[cfg(unix)]
fn ns_to_timespec(n: i64) -> libc::timespec {
    libc::timespec {
        tv_sec: n.div_euclid(1_000_000_000) as libc::time_t,
        tv_nsec: n.rem_euclid(1_000_000_000) as _,
    }
}

#[cfg(unix)]
fn secs_to_timespec(s: f64) -> libc::timespec {
    let sec = s.floor();
    let nsec = ((s - sec) * 1e9).round() as i64;
    libc::timespec {
        tv_sec: sec as libc::time_t,
        tv_nsec: nsec.clamp(0, 999_999_999) as _,
    }
}

/// The process-wide `os.PathLike` ABC type. Memoised so its identity is
/// stable across module rebuilds and so `isinstance(x, os.PathLike)` can
/// recognise it (and apply the `__fspath__` structural check, like CPython's
/// `PathLike.__subclasshook__`).
pub fn path_like_type() -> Rc<crate::types::TypeObject> {
    static CLS: std::sync::OnceLock<Rc<crate::types::TypeObject>> = std::sync::OnceLock::new();
    CLS.get_or_init(|| path_like_type_singleton("PathLike"))
        .clone()
}

fn path_like_type_singleton(name: &str) -> Rc<crate::types::TypeObject> {
    use crate::builtin_types::builtin_types;
    use crate::object::{BuiltinFn, MethodWrapper};
    use crate::types::{TypeFlags, TypeObject};
    let bt = builtin_types();
    let mut dict = DictData::new();
    // `os.PathLike` is an ABC; `os.PathLike.register(C)` marks `C` as a virtual
    // subclass (CPython's `pathlib._local` does `os.PathLike.register(PurePath)`
    // at import). Membership here is checked structurally (any `__fspath__`),
    // so `register` just needs to exist and return its argument so the
    // `@PathLike.register` decorator form works.
    dict.insert(
        DictKey(Object::from_static("register")),
        Object::ClassMethod(MethodWrapper::new(Object::Builtin(Rc::new(BuiltinFn {
            name: "register",
            binds_instance: true,
            call: Box::new(|args| Ok(args.get(1).cloned().unwrap_or(Object::None))),
            call_kw: None,
        })))),
    );
    TypeObject::new_with_flags(
        Box::leak(name.to_owned().into_boxed_str()),
        vec![bt.object_.clone()],
        dict,
        TypeFlags {
            is_exception: false,
            is_builtin: true,
        },
    )
    .expect("os.PathLike")
}

/// The "visible" struct-sequence members of `os.stat_result`, in index order
/// — the first 10 positions `stat_result(seq)` consumes and `st[i]` returns,
/// matching CPython's `structseq` layout (`Modules/posixmodule.c`).
const STAT_RESULT_FIELDS: [&str; 10] = [
    "st_mode", "st_ino", "st_dev", "st_nlink", "st_uid", "st_gid", "st_size", "st_atime",
    "st_mtime", "st_ctime",
];

/// Process-wide memoised `os.stat_result` type. Memoisation is load-bearing
/// for *identity*: `stat`/`lstat`/`fstat`/`DirEntry.stat()` build instances of
/// this exact type, and the module exposes the very same object as
/// `os.stat_result` / `posix.stat_result`, so `isinstance(os.stat(p),
/// os.stat_result)` holds — the CPython invariant tests (and `tarfile`,
/// `shutil`, `http.server`, …) rely on. The type is a CPython-style struct
/// sequence: addressable both by `st_*` attribute and by integer index, and
/// constructible from a 10-sequence (`posix.stat_result((...))`).
fn stat_result_type() -> Rc<crate::types::TypeObject> {
    struct_seq_type("stat_result", "os", &STAT_RESULT_FIELDS)
}

/// `os.terminal_size` — a 2-field struct sequence (`columns`, `lines`). Verbatim
/// `shutil.get_terminal_size()` (and hence `argparse`'s `HelpFormatter`) builds
/// and reads these by attribute (`size.columns`) *and* constructs them from a
/// fallback 2-tuple (`os.terminal_size(fallback)`), so it must be a real struct
/// sequence rather than a bare tuple.
fn terminal_size_type() -> Rc<crate::types::TypeObject> {
    const TERMINAL_SIZE_FIELDS: [&str; 2] = ["columns", "lines"];
    struct_seq_type("terminal_size", "os", &TERMINAL_SIZE_FIELDS)
}

/// Build (and memoise, by `name`) a CPython-style `PyStructSequence` type:
/// addressable both by `fields[i]` attribute and by integer index, with
/// `__len__` == `fields.len()`, and constructible from a `>= fields.len()`
/// sequence plus an optional trailing dict of hidden named fields. Backs
/// `os.stat_result`, `os.terminal_size`, etc. Memoisation keeps type identity
/// stable across module rebuilds so `isinstance` holds.
pub(crate) fn struct_seq_type(
    name: &'static str,
    module: &'static str,
    fields: &'static [&'static str],
) -> Rc<crate::types::TypeObject> {
    use crate::types::{TypeFlags, TypeObject};
    use std::collections::HashMap;
    thread_local! {
        static REGISTRY: RefCell<HashMap<&'static str, Rc<TypeObject>>> =
            RefCell::new(HashMap::new());
    }
    REGISTRY.with(|reg| {
        if let Some(c) = reg.borrow().get(name) {
            return c.clone();
        }
        let bt = crate::builtin_types::builtin_types();
        let mut dict = DictData::new();
        // `__module__`/`__qualname__` let `pickle`/`copy` find the type by
        // reference (e.g. `os.stat_result`) instead of guessing `builtins`.
        dict.insert(
            DictKey(Object::from_static("__module__")),
            Object::from_static(module),
        );
        dict.insert(
            DictKey(Object::from_static("__qualname__")),
            Object::from_static(name),
        );
        struct_seq_method(&mut dict, "__init__", move |args| {
            struct_seq_init(name, fields, args)
        });
        // `__reduce__` makes the struct sequence picklable as
        // `(type, (visible_tuple, hidden_dict))` — CPython's `structseq_reduce`.
        struct_seq_method(&mut dict, "__reduce__", move |args| {
            struct_seq_reduce(name, module, fields, args)
        });
        struct_seq_method(&mut dict, "__getitem__", move |args| {
            struct_seq_getitem(fields, args)
        });
        struct_seq_method(&mut dict, "__len__", move |_args| {
            Ok(Object::Int(fields.len() as i64))
        });
        // CPython struct sequences subclass `tuple`, so `==`/`!=`/`hash()`
        // compare the visible fields by value (e.g. `os.stat(a) == os.stat(a)`
        // in `test_pathlib`, and using a `stat_result` as a dict key). Compare
        // against another struct sequence of the same type or a plain tuple.
        struct_seq_method(&mut dict, "__eq__", move |args| {
            struct_seq_richcompare(fields, args, CompareKind::Eq)
        });
        struct_seq_method(&mut dict, "__ne__", move |args| {
            struct_seq_richcompare(fields, args, CompareKind::NotEq)
        });
        struct_seq_method(&mut dict, "__hash__", move |args| {
            struct_seq_hash(fields, args)
        });
        let cls = TypeObject::new_with_flags(
            name,
            vec![bt.object_.clone()],
            dict,
            TypeFlags {
                is_exception: false,
                is_builtin: true,
            },
        )
        .expect("struct sequence type");
        reg.borrow_mut().insert(name, cls.clone());
        cls
    })
}

fn struct_seq_method<F>(dict: &mut DictData, name: &'static str, body: F)
where
    F: Fn(&[Object]) -> Result<Object, RuntimeError> + Send + Sync + 'static,
{
    dict.insert(
        DictKey(Object::from_static(name)),
        Object::Builtin(Rc::new(crate::object::BuiltinFn {
            name,
            binds_instance: true,
            call: Box::new(body),
            call_kw: None,
        })),
    );
}

/// `T(sequence[, dict])` — CPython accepts a `>= len(fields)` element sequence
/// (the visible fields) plus an optional dict of hidden named fields. Tests
/// fabricate stat results this way to drive `posixpath.ismount`, `shutil`
/// device checks, etc.
fn struct_seq_init(
    name: &'static str,
    fields: &'static [&'static str],
    args: &[Object],
) -> Result<Object, RuntimeError> {
    let Some(Object::Instance(inst)) = args.first() else {
        return Err(type_error(format!(
            "{name}.__init__ requires a {name} instance"
        )));
    };
    let seq = args
        .get(1)
        .ok_or_else(|| type_error(format!("{name}() missing required argument: 'sequence'")))?;
    let values = match seq {
        Object::Tuple(items) => items.to_vec(),
        Object::List(items) => items.borrow().clone(),
        other => {
            let mut it = other
                .make_iter()
                .map_err(|_| type_error(format!("{name}() argument must be a sequence")))?;
            let mut v = Vec::new();
            while let Some(x) = it.next_value() {
                v.push(x);
            }
            v
        }
    };
    if values.len() < fields.len() {
        return Err(type_error(format!(
            "{name}() takes a {}-sequence ({}-sequence given)",
            fields.len(),
            values.len()
        )));
    }
    {
        let mut d = inst.dict.borrow_mut();
        for (field, value) in fields.iter().zip(values.iter()) {
            d.insert(DictKey(Object::from_static(field)), value.clone());
        }
    }
    // Optional second positional: a dict of named hidden fields. Snapshot the
    // pairs before borrowing `inst.dict` mutably to avoid a double borrow if
    // the same Rc backs both (it never does here, but keeps this panic-free).
    if let Some(Object::Dict(extra)) = args.get(2) {
        let pairs: Vec<(Object, Object)> = extra
            .borrow()
            .iter()
            .map(|(k, v)| (k.0.clone(), v.clone()))
            .collect();
        let mut d = inst.dict.borrow_mut();
        for (k, v) in pairs {
            d.insert(DictKey(k), v);
        }
    }
    Ok(Object::None)
}

fn struct_seq_getitem(
    fields: &'static [&'static str],
    args: &[Object],
) -> Result<Object, RuntimeError> {
    let Some(Object::Instance(inst)) = args.first() else {
        return Err(type_error("struct sequence indexing requires an instance"));
    };
    let field = |i: usize| -> Object {
        let v = inst
            .dict
            .borrow()
            .get(&DictKey(Object::from_static(fields[i])))
            .cloned()
            .unwrap_or(Object::Int(0));
        struct_seq_slot(fields[i], v)
    };
    // CPython struct sequences are tuple-backed, so slicing yields a plain
    // `tuple` of the selected fields (e.g. `time.localtime()[:6]`, which
    // `tarfile`/`zipfile` use to build DOS timestamps).
    if let Some(Object::Slice(s)) = args.get(1) {
        let idxs = crate::slice_indices(fields.len(), s)?;
        return Ok(Object::new_tuple(idxs.into_iter().map(field).collect()));
    }
    let idx = args
        .get(1)
        .and_then(Object::as_i64)
        .ok_or_else(|| type_error("struct sequence indices must be integers"))?;
    let n = fields.len() as i64;
    let i = if idx < 0 { idx + n } else { idx };
    if i < 0 || i >= n {
        return Err(crate::error::index_error("tuple index out of range"));
    }
    Ok(field(i as usize))
}

/// Read the visible (sequence) field values of a struct-sequence instance,
/// in declaration order, defaulting absent fields to `0` (as the tuple slot
/// would be).
fn struct_seq_values(
    fields: &'static [&'static str],
    inst: &Rc<crate::types::PyInstance>,
) -> Vec<Object> {
    let d = inst.dict.borrow();
    fields
        .iter()
        .map(|f| {
            let v = d
                .get(&DictKey(Object::from_static(f)))
                .cloned()
                .unwrap_or(Object::Int(0));
            struct_seq_slot(f, v)
        })
        .collect()
}

/// `__reduce__` for a struct sequence: `(type, (visible_tuple, hidden_dict))`.
///
/// Mirrors CPython's `structseq_reduce`. The visible tuple carries the
/// sequence slots (integer `st_*time`s for `stat_result`); the hidden dict
/// carries every *named-only* member plus the float `st_atime`/`st_mtime`/
/// `st_ctime` values that the integer slots can't reconstruct. On unpickling,
/// `struct_seq_init(type, (seq, dict))` restores both.
fn struct_seq_reduce(
    name: &'static str,
    module: &'static str,
    fields: &'static [&'static str],
    args: &[Object],
) -> Result<Object, RuntimeError> {
    let Some(Object::Instance(inst)) = args.first() else {
        return Err(type_error("struct sequence reduce requires an instance"));
    };
    let visible = Object::new_tuple(struct_seq_values(fields, inst));
    let extra = Rc::new(RefCell::new(DictData::new()));
    {
        let d = inst.dict.borrow();
        let mut e = extra.borrow_mut();
        for (k, v) in d.iter() {
            let keep = match &k.0 {
                Object::Str(s) => {
                    let ks = s.to_string();
                    let ks = ks.as_str();
                    !fields.iter().any(|f| *f == ks)
                        || matches!(ks, "st_atime" | "st_mtime" | "st_ctime")
                }
                _ => true,
            };
            if keep {
                e.insert(DictKey(k.0.clone()), v.clone());
            }
        }
    }
    let cls = Object::Type(struct_seq_type(name, module, fields));
    Ok(Object::new_tuple(vec![
        cls,
        Object::new_tuple(vec![visible, Object::Dict(extra)]),
    ]))
}

/// Map a struct-sequence field to its *sequence-slot* representation.
///
/// CPython's `os.stat_result` is the canonical example: the named attributes
/// `st_atime`/`st_mtime`/`st_ctime` are floats, but the corresponding tuple
/// slots (`st[7..10]`, and therefore `tuple(st)`, hashing and comparison)
/// hold the *integer* seconds. Everything else passes through unchanged.
fn struct_seq_slot(field: &str, value: Object) -> Object {
    if matches!(field, "st_atime" | "st_mtime" | "st_ctime") {
        if let Object::Float(f) = value {
            return Object::Int(f as i64);
        }
    }
    value
}

/// `__eq__`/`__ne__` for struct sequences: compare the visible fields as a
/// tuple against another instance of the *same* struct-sequence type or a
/// plain `tuple`/`list`. Anything else yields `NotImplemented` so the other
/// operand gets a chance (matching tuple semantics).
fn struct_seq_richcompare(
    fields: &'static [&'static str],
    args: &[Object],
    op: CompareKind,
) -> Result<Object, RuntimeError> {
    let Some(Object::Instance(inst)) = args.first() else {
        return Err(type_error(
            "struct sequence comparison requires an instance",
        ));
    };
    let self_tuple = Object::new_tuple(struct_seq_values(fields, inst));
    let other = match args.get(1) {
        Some(Object::Instance(other_inst)) if Rc::ptr_eq(&inst.cls(), &other_inst.cls()) => {
            Object::new_tuple(struct_seq_values(fields, other_inst))
        }
        Some(t @ Object::Tuple(_)) => t.clone(),
        Some(Object::List(items)) => Object::new_tuple(items.borrow().clone()),
        _ => return Ok(crate::vm_singletons::not_implemented()),
    };
    let ptr = crate::vm_singletons::current_interpreter_ptr()
        .ok_or_else(|| type_error("struct sequence comparison: no active interpreter"))?;
    // SAFETY: published by the enclosing VM frame on this thread.
    let interp = unsafe { &mut *ptr };
    Ok(Object::Bool(interp.op_compare(&self_tuple, &other, op)?))
}

/// `__hash__` for struct sequences: hash the visible fields as a tuple, so a
/// `stat_result` hashes like `tuple(stat_result)` (CPython relies on this).
fn struct_seq_hash(
    fields: &'static [&'static str],
    args: &[Object],
) -> Result<Object, RuntimeError> {
    let Some(Object::Instance(inst)) = args.first() else {
        return Err(type_error("struct sequence hash requires an instance"));
    };
    let tuple = Object::new_tuple(struct_seq_values(fields, inst));
    crate::builtins::hash_object(&tuple)
}

/// Build an instance of a [`struct_seq_type`], binding `values` positionally to
/// `fields`. Surplus `values` are ignored; missing trailing ones simply aren't
/// set (callers pass a full row). Shared by `time.struct_time`, `os.times_result`,
/// etc., so they all get attribute + index access for free.
pub(crate) fn struct_seq_instance(
    ty: Rc<crate::types::TypeObject>,
    fields: &'static [&'static str],
    values: Vec<Object>,
) -> Object {
    let inst = crate::types::PyInstance::new(ty);
    {
        let mut d = inst.dict.borrow_mut();
        for (field, value) in fields.iter().zip(values) {
            d.insert(DictKey(Object::from_static(field)), value);
        }
    }
    Object::Instance(Rc::new(inst))
}

/// Construct an `os.terminal_size` instance with the given dimensions.
fn make_terminal_size(columns: i64, lines: i64) -> Object {
    struct_seq_instance(
        terminal_size_type(),
        &["columns", "lines"],
        vec![Object::Int(columns), Object::Int(lines)],
    )
}

// ---------- os.path ----------

fn as_str(obj: &Object, func: &str) -> Result<String, RuntimeError> {
    match obj {
        Object::Str(s) => Ok(s.to_string()),
        _ => Err(type_error(format!(
            "{func}() argument must be str, not '{}'",
            obj.type_name()
        ))),
    }
}

fn path_join(args: &[Object]) -> Result<Object, RuntimeError> {
    let mut path = PathBuf::new();
    for (i, arg) in args.iter().enumerate() {
        let s = as_str(arg, "join")?;
        if i == 0 {
            path.push(&s);
        } else {
            let p = Path::new(&s);
            if p.is_absolute() {
                path = p.to_path_buf();
            } else {
                path.push(p);
            }
        }
    }
    Ok(Object::from_str(path.to_string_lossy().into_owned()))
}

fn path_split(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = first_path(args, "split")?;
    let p = PathBuf::from(&s);
    let head = p
        .parent()
        .map_or(String::new(), |x| x.to_string_lossy().into_owned());
    let tail = p
        .file_name()
        .map_or(String::new(), |x| x.to_string_lossy().into_owned());
    Ok(Object::new_tuple(vec![
        Object::from_str(head),
        Object::from_str(tail),
    ]))
}

fn path_splitext(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = first_path(args, "splitext")?;
    if let Some(dot) = find_ext_dot(&s) {
        let (root, ext) = s.split_at(dot);
        Ok(Object::new_tuple(vec![
            Object::from_str(root.to_owned()),
            Object::from_str(ext.to_owned()),
        ]))
    } else {
        Ok(Object::new_tuple(vec![
            Object::from_str(s),
            Object::from_static(""),
        ]))
    }
}

/// `os.path.splitdrive(p)` — on POSIX the drive component is always empty,
/// so this returns `("", p)` (matching `posixpath.splitdrive`). Paths here
/// are already `str` by the time callers reach this (e.g. `mimetypes`
/// `fsdecode`s first), so we reuse the `first_path` string coercion.
fn path_splitdrive(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = first_path(args, "splitdrive")?;
    Ok(Object::new_tuple(vec![
        Object::from_static(""),
        Object::from_str(s),
    ]))
}

/// Mirror CPython's `os.path.splitext`: split on the *last* dot, but
/// only when that dot follows a non-dot character (`.profile` keeps
/// the leading dot).
fn find_ext_dot(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut i = bytes.len();
    while i > 0 {
        i -= 1;
        let c = bytes[i];
        if c == b'/' || (cfg!(windows) && c == b'\\') {
            return None;
        }
        if c == b'.' {
            // Skip leading-dot files (`.bashrc`) and dot runs.
            if i == 0 {
                return None;
            }
            let prev = bytes[i - 1];
            if prev == b'/' || (cfg!(windows) && prev == b'\\') {
                return None;
            }
            if prev == b'.' {
                continue;
            }
            return Some(i);
        }
    }
    None
}

fn path_basename(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = first_path(args, "basename")?;
    let name = Path::new(&s)
        .file_name()
        .map_or(String::new(), |x| x.to_string_lossy().into_owned());
    Ok(Object::from_str(name))
}

fn path_dirname(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = first_path(args, "dirname")?;
    let dir = Path::new(&s)
        .parent()
        .map_or(String::new(), |x| x.to_string_lossy().into_owned());
    Ok(Object::from_str(dir))
}

fn path_exists(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = first_path(args, "exists")?;
    Ok(Object::Bool(Path::new(&s).exists()))
}

fn path_lexists(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = first_path(args, "lexists")?;
    // lexists() uses lstat(): it returns True even for a broken symlink,
    // so probe with symlink_metadata rather than following the link.
    Ok(Object::Bool(std::fs::symlink_metadata(&s).is_ok()))
}

fn path_isfile(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = first_path(args, "isfile")?;
    Ok(Object::Bool(Path::new(&s).is_file()))
}

fn path_isdir(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = first_path(args, "isdir")?;
    Ok(Object::Bool(Path::new(&s).is_dir()))
}

fn path_abspath(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = first_path(args, "abspath")?;
    let p = PathBuf::from(&s);
    let abs = if p.is_absolute() {
        p
    } else {
        std::env::current_dir()
            .map_err(|e| os_error(format!("abspath: {e}")))?
            .join(p)
    };
    Ok(Object::from_str(abs.to_string_lossy().into_owned()))
}

/// `os.path.realpath` — resolve symlinks via `fs::canonicalize`
/// (CPython's non-strict mode: a nonexistent tail rides lexically on
/// the longest resolvable prefix).
fn path_realpath(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = first_path(args, "realpath")?;
    let p = PathBuf::from(&s);
    let abs = if p.is_absolute() {
        p
    } else {
        std::env::current_dir()
            .map_err(|e| os_error(format!("realpath: {e}")))?
            .join(p)
    };
    if let Ok(c) = std::fs::canonicalize(&abs) {
        return Ok(Object::from_str(c.to_string_lossy().into_owned()));
    }
    let mut prefix = abs.clone();
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    while prefix.file_name().is_some() {
        if let Ok(c) = std::fs::canonicalize(&prefix) {
            let mut out = c;
            for t in tail.iter().rev() {
                out.push(t);
            }
            return Ok(Object::from_str(normpath_lexical(&out.to_string_lossy())));
        }
        tail.push(prefix.file_name().expect("checked above").to_owned());
        prefix.pop();
    }
    Ok(Object::from_str(normpath_lexical(&abs.to_string_lossy())))
}

fn path_normpath(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = first_path(args, "normpath")?;
    let normalised = normpath_lexical(&s);
    Ok(Object::from_str(normalised))
}

fn path_normcase(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = first_path(args, "normcase")?;
    // On Windows, normcase lowercases the entire path and rewrites
    // forward slashes. Elsewhere it's a no-op.
    if cfg!(windows) {
        let out: String = s
            .chars()
            .map(|c| {
                if c == '/' {
                    '\\'
                } else {
                    c.to_ascii_lowercase()
                }
            })
            .collect();
        Ok(Object::from_str(out))
    } else {
        Ok(Object::from_str(s))
    }
}

fn path_expanduser(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = first_path(args, "expanduser")?;
    if !s.starts_with('~') {
        return Ok(Object::from_str(s));
    }
    let home = std::env::var("HOME").unwrap_or_default();
    if home.is_empty() {
        return Ok(Object::from_str(s));
    }
    if s == "~" {
        return Ok(Object::from_str(home));
    }
    if s.starts_with("~/") {
        return Ok(Object::from_str(format!("{}{}", home, &s[1..])));
    }
    Ok(Object::from_str(s))
}

fn path_expandvars(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = first_path(args, "expandvars")?;
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '$' {
            let mut name = String::new();
            // Support ${VAR} and $VAR
            if chars.peek() == Some(&'{') {
                chars.next();
                while let Some(&nc) = chars.peek() {
                    if nc == '}' {
                        chars.next();
                        break;
                    }
                    name.push(nc);
                    chars.next();
                }
            } else {
                while let Some(&nc) = chars.peek() {
                    if nc.is_ascii_alphanumeric() || nc == '_' {
                        name.push(nc);
                        chars.next();
                    } else {
                        break;
                    }
                }
            }
            if name.is_empty() {
                out.push('$');
            } else if let Ok(value) = std::env::var(&name) {
                out.push_str(&value);
            } else {
                out.push('$');
                out.push_str(&name);
            }
        } else {
            out.push(c);
        }
    }
    Ok(Object::from_str(out))
}

fn path_isabs(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = first_path(args, "isabs")?;
    Ok(Object::Bool(std::path::Path::new(&s).is_absolute()))
}

fn path_relpath(args: &[Object]) -> Result<Object, RuntimeError> {
    let path = first_path(args, "relpath")?;
    let start = match args.get(1) {
        Some(o) => as_str(o, "relpath")?,
        None => std::env::current_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| ".".to_owned()),
    };
    let path_abs = std::path::Path::new(&path).canonicalize();
    let start_abs = std::path::Path::new(&start).canonicalize();
    if let (Ok(p), Ok(s)) = (path_abs, start_abs) {
        if let Ok(rel) = p.strip_prefix(&s) {
            let mut r = rel.display().to_string();
            if r.is_empty() {
                r = ".".to_owned();
            }
            return Ok(Object::from_str(r));
        }
    }
    Ok(Object::from_str(path))
}

fn path_commonpath(args: &[Object]) -> Result<Object, RuntimeError> {
    let paths_obj = args
        .first()
        .ok_or_else(|| type_error("commonpath() requires an iterable of paths"))?;
    let parts: Vec<String> = match paths_obj {
        Object::List(l) => l.borrow().iter().map(|o| o.to_str()).collect(),
        Object::Tuple(t) => t.iter().map(|o| o.to_str()).collect(),
        _ => return Err(type_error("commonpath() requires a list or tuple of paths")),
    };
    if parts.is_empty() {
        return Err(crate::error::value_error("commonpath() arg is empty"));
    }
    let sep = if cfg!(windows) { '\\' } else { '/' };
    let split = |s: &str| -> Vec<String> { s.split(sep).map(str::to_owned).collect() };
    let lists: Vec<Vec<String>> = parts.iter().map(|s| split(s)).collect();
    let min_len = lists.iter().map(|v| v.len()).min().unwrap();
    let mut common: Vec<String> = Vec::new();
    for i in 0..min_len {
        let token = &lists[0][i];
        if lists.iter().all(|v| &v[i] == token) {
            common.push(token.clone());
        } else {
            break;
        }
    }
    Ok(Object::from_str(common.join(&sep.to_string())))
}

fn path_commonprefix(args: &[Object]) -> Result<Object, RuntimeError> {
    let paths_obj = args
        .first()
        .ok_or_else(|| type_error("commonprefix() requires an iterable of paths"))?;
    let parts: Vec<String> = match paths_obj {
        Object::List(l) => l.borrow().iter().map(|o| o.to_str()).collect(),
        Object::Tuple(t) => t.iter().map(|o| o.to_str()).collect(),
        _ => {
            return Err(type_error(
                "commonprefix() requires a list or tuple of paths",
            ))
        }
    };
    if parts.is_empty() {
        return Ok(Object::from_str(""));
    }
    let first = &parts[0];
    let mut end = first.len();
    for s in &parts[1..] {
        let limit = end.min(s.len());
        let mut i = 0;
        let a = first.as_bytes();
        let b = s.as_bytes();
        while i < limit && a[i] == b[i] {
            i += 1;
        }
        end = i;
        if end == 0 {
            break;
        }
    }
    Ok(Object::from_str(first[..end].to_owned()))
}

fn path_getsize(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = first_path(args, "getsize")?;
    let md = std::fs::metadata(&s).map_err(|e| crate::error::os_error(format!("{}: {}", s, e)))?;
    Ok(Object::Int(md.len() as i64))
}

fn path_getmtime(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = first_path(args, "getmtime")?;
    let md = std::fs::metadata(&s).map_err(|e| crate::error::os_error(format!("{}: {}", s, e)))?;
    let mtime = md
        .modified()
        .map_err(|e| crate::error::os_error(e.to_string()))?;
    let secs = mtime
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    Ok(Object::Float(secs))
}

fn path_getctime(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = first_path(args, "getctime")?;
    let md = std::fs::metadata(&s).map_err(|e| crate::error::os_error(format!("{}: {}", s, e)))?;
    // `created` is unreliable across platforms; fall back to mtime.
    let ct = md
        .created()
        .or_else(|_| md.modified())
        .map_err(|e| crate::error::os_error(e.to_string()))?;
    let secs = ct
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    Ok(Object::Float(secs))
}

fn path_islink(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = first_path(args, "islink")?;
    let md = std::fs::symlink_metadata(&s);
    Ok(Object::Bool(
        matches!(md, Ok(m) if m.file_type().is_symlink()),
    ))
}

fn path_samefile(args: &[Object]) -> Result<Object, RuntimeError> {
    let a = first_path(args, "samefile")?;
    let b = match args.get(1) {
        Some(o) => as_str(o, "samefile")?,
        None => return Err(type_error("samefile() requires two paths")),
    };
    let am = std::fs::metadata(&a);
    let bm = std::fs::metadata(&b);
    match (am, bm) {
        (Ok(am), Ok(bm)) => {
            // On Unix the dev+inode identifies a file; on Windows
            // we approximate by comparing canonical paths.
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                Ok(Object::Bool(am.dev() == bm.dev() && am.ino() == bm.ino()))
            }
            #[cfg(not(unix))]
            {
                let _ = (am, bm);
                let acanon = std::path::Path::new(&a).canonicalize();
                let bcanon = std::path::Path::new(&b).canonicalize();
                Ok(Object::Bool(
                    matches!((acanon, bcanon), (Ok(ac), Ok(bc)) if ac == bc),
                ))
            }
        }
        _ => Ok(Object::Bool(false)),
    }
}

fn first_path(args: &[Object], func: &str) -> Result<String, RuntimeError> {
    match args.first() {
        Some(obj) => path_to_string(obj, func),
        None => Err(type_error(format!("{func}() requires a path argument"))),
    }
}

/// Resolve the `n`-th positional argument as a path (str/bytes/`os.PathLike`).
/// Used for the *second* path of two-path calls (`symlink`/`link`/`rename`),
/// which must honour `PathLike` exactly like the first (`pathlib.Path`s flow
/// through here once they're real `os.PathLike`s).
fn nth_path(args: &[Object], n: usize, func: &str) -> Result<String, RuntimeError> {
    match args.get(n) {
        Some(obj) => path_to_string(obj, func),
        None => Err(type_error(format!("{func}() missing path argument"))),
    }
}

/// Reduce a path argument to a `str`, accepting `str`, `bytes`/`bytearray`,
/// and `os.PathLike` objects (resolved through `__fspath__`) — matching
/// CPython's `path_converter`. Shared by the `os.*` filesystem entry points.
pub(crate) fn path_to_string(obj: &Object, func: &str) -> Result<String, RuntimeError> {
    let s = match obj {
        Object::Str(s) => s.to_string(),
        Object::Bytes(b) => String::from_utf8_lossy(b).into_owned(),
        Object::ByteArray(b) => String::from_utf8_lossy(&b.borrow()).into_owned(),
        Object::Instance(_) => {
            let ptr = crate::vm_singletons::current_interpreter_ptr().ok_or_else(|| {
                type_error(format!(
                    "{func}: path should be string, bytes or os.PathLike, not {}",
                    obj.type_name()
                ))
            })?;
            // SAFETY: published by the enclosing VM frame on this thread.
            let interp = unsafe { &mut *ptr };
            let fspath = interp.load_attr_public(obj, "__fspath__").map_err(|_| {
                type_error(format!(
                    "{func}: path should be string, bytes or os.PathLike, not {}",
                    obj.type_name()
                ))
            })?;
            match interp.call_object(fspath, &[], &[])? {
                Object::Str(s) => s.to_string(),
                Object::Bytes(b) => String::from_utf8_lossy(&b).into_owned(),
                other => {
                    return Err(type_error(format!(
                        "expected {func}.__fspath__() to return str or bytes, not {}",
                        other.type_name()
                    )))
                }
            }
        }
        other => {
            return Err(type_error(format!(
                "{func}: path should be string, bytes or os.PathLike, not {}",
                other.type_name()
            )))
        }
    };
    // A NUL in a path is invalid at the syscall boundary; CPython's
    // `path_converter` raises `ValueError` rather than truncating
    // (`os.stat('/\x00')`, `realpath('/\x00', strict=True)`).
    if s.as_bytes().contains(&0) {
        return Err(value_error("embedded null byte"));
    }
    Ok(s)
}

/// Lexical path normalisation matching CPython's `os.path.normpath`:
/// drops `.` components, collapses `..` against earlier parts, and
/// collapses redundant separators. Does not touch the filesystem.
fn normpath_lexical(s: &str) -> String {
    let sep_str = if cfg!(windows) { "\\" } else { "/" };
    let is_sep = |c: char| c == '/' || (cfg!(windows) && c == '\\');
    let is_abs = s.starts_with(is_sep);
    let mut stack: Vec<&str> = Vec::new();
    for part in s.split(is_sep) {
        match part {
            "" | "." => continue,
            ".." => {
                if let Some(top) = stack.last() {
                    if *top != ".." {
                        stack.pop();
                        continue;
                    }
                }
                if !is_abs {
                    stack.push("..");
                }
            }
            other => stack.push(other),
        }
    }
    let mut out = if is_abs {
        sep_str.to_owned()
    } else {
        String::new()
    };
    for (i, p) in stack.iter().enumerate() {
        if i > 0 || (i == 0 && !is_abs) {
            if i > 0 {
                out.push_str(sep_str);
            }
        }
        out.push_str(p);
    }
    if out.is_empty() {
        ".".to_owned()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitext_handles_simple_extensions() {
        let s = "foo.txt".to_owned();
        assert_eq!(find_ext_dot(&s), Some(3));
        let s = "foo".to_owned();
        assert_eq!(find_ext_dot(&s), None);
        let s = ".bashrc".to_owned();
        assert_eq!(find_ext_dot(&s), None);
        let s = "a/b/c.gz".to_owned();
        assert_eq!(find_ext_dot(&s), Some(5));
    }

    #[test]
    fn normpath_collapses_dots() {
        // `normpath_lexical` mirrors CPython: `ntpath.normpath` joins
        // with `\` on Windows, `posixpath.normpath` with `/` elsewhere.
        let sep = std::path::MAIN_SEPARATOR;
        assert_eq!(normpath_lexical("a/./b"), format!("a{sep}b"));
        assert_eq!(normpath_lexical("a/b/../c"), format!("a{sep}c"));
        assert_eq!(normpath_lexical("./"), ".");
    }
}
