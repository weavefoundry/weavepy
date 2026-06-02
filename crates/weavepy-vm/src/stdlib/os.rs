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

use crate::error::{os_error, type_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};

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
            DictKey(Object::from_static("path")),
            Object::Module(path_mod),
        );
        d.insert(DictKey(Object::from_static("environ")), initial_environ());

        d.insert(
            DictKey(Object::from_static("getcwd")),
            builtin("getcwd", os_getcwd),
        );
        d.insert(
            DictKey(Object::from_static("getenv")),
            builtin("getenv", os_getenv),
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
            DictKey(Object::from_static("stat")),
            builtin("stat", os_stat_stub),
        );
        d.insert(
            DictKey(Object::from_static("lstat")),
            builtin("lstat", os_lstat),
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
            builtin("walk", os_walk),
        );
        d.insert(
            DictKey(Object::from_static("scandir")),
            builtin("scandir", os_scandir),
        );
        d.insert(
            DictKey(Object::from_static("pipe")),
            builtin("pipe", os_pipe),
        );
        d.insert(DictKey(Object::from_static("dup")), builtin("dup", os_dup));
        d.insert(
            DictKey(Object::from_static("dup2")),
            builtin("dup2", os_dup2),
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
            DictKey(Object::from_static("kill")),
            builtin("kill", os_kill),
        );
        d.insert(
            DictKey(Object::from_static("waitpid")),
            builtin("waitpid", os_waitpid),
        );
        // Common signal numbers — match libc on POSIX.
        d.insert(DictKey(Object::from_static("SIGTERM")), Object::Int(15));
        d.insert(DictKey(Object::from_static("SIGKILL")), Object::Int(9));
        d.insert(DictKey(Object::from_static("SIGINT")), Object::Int(2));
        d.insert(DictKey(Object::from_static("SIGHUP")), Object::Int(1));
        d.insert(DictKey(Object::from_static("WNOHANG")), Object::Int(1));
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
        d.insert(
            DictKey(Object::from_static("umask")),
            builtin("umask", os_umask),
        );
        d.insert(
            DictKey(Object::from_static("symlink")),
            builtin("symlink", os_symlink),
        );
        d.insert(
            DictKey(Object::from_static("link")),
            builtin("link", os_link),
        );
        d.insert(
            DictKey(Object::from_static("chmod")),
            builtin("chmod", os_chmod),
        );
        d.insert(
            DictKey(Object::from_static("utime")),
            builtin("utime", os_utime),
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
            builtin("realpath", path_abspath),
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

fn builtin(name: &'static str, body: fn(&[Object]) -> Result<Object, RuntimeError>) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        call: Box::new(body),
        call_kw: None,
    }))
}

/// As [`builtin`], but the body also takes a keyword-argument list.
/// Use this for surfaces where CPython exposes named parameters
/// (e.g. `os.makedirs(path, mode=0o777, exist_ok=False)`).
fn builtin_kw(
    name: &'static str,
    body: fn(&[Object], &[(String, Object)]) -> Result<Object, RuntimeError>,
) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        call: Box::new(move |args| body(args, &[])),
        call_kw: Some(Box::new(body)),
    }))
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
    std::fs::create_dir(&p).map_err(|e| crate::error::io_error_to_py(&e))?;
    Ok(Object::None)
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
    let dst = match args.get(1) {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Err(type_error("rename() second arg must be str")),
    };
    std::fs::rename(&src, &dst).map_err(|e| crate::error::io_error_to_py(&e))?;
    Ok(Object::None)
}

fn os_listdir(args: &[Object]) -> Result<Object, RuntimeError> {
    let p = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        None => ".".to_string(),
        _ => return Err(type_error("listdir() arg must be str")),
    };
    let mut out = Vec::new();
    let iter = std::fs::read_dir(&p).map_err(|e| crate::error::io_error_to_py(&e))?;
    for entry in iter {
        let entry = entry.map_err(|e| crate::error::io_error_to_py(&e))?;
        out.push(Object::from_str(
            entry.file_name().to_string_lossy().into_owned(),
        ));
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

fn os_open_stub(_args: &[Object]) -> Result<Object, RuntimeError> {
    Err(crate::error::not_implemented_error(
        "os.open(): raw fd interface is not implemented in WeavePy yet",
    ))
}

fn os_stat_stub(args: &[Object]) -> Result<Object, RuntimeError> {
    let p = first_path(args, "stat")?;
    let meta = std::fs::metadata(&p).map_err(|e| crate::error::io_error_to_py(&e))?;
    Ok(stat_result_from_meta(&meta))
}

fn os_lstat(args: &[Object]) -> Result<Object, RuntimeError> {
    let p = first_path(args, "lstat")?;
    let meta = std::fs::symlink_metadata(&p).map_err(|e| crate::error::io_error_to_py(&e))?;
    Ok(stat_result_from_meta(&meta))
}

fn stat_result_from_meta(meta: &std::fs::Metadata) -> Object {
    use crate::types::PyInstance;
    let ty = path_like_type_singleton("stat_result");
    let inst = PyInstance::new(ty);
    let mut d = inst.dict.borrow_mut();
    let kind_bits: i64 = if meta.is_dir() {
        0o040_000
    } else if meta.is_file() {
        0o100_000
    } else {
        0o120_000
    };
    // The permission bits live in the low 9 bits of `st_mode`. On
    // Unix we read them from the filesystem; on platforms without
    // `PermissionsExt` we fall back to the historical hard-coded
    // values so callers that just want to test directory/file
    // shape still see something sensible.
    #[cfg(unix)]
    let perm_bits: i64 = {
        use std::os::unix::fs::PermissionsExt;
        i64::from(meta.permissions().mode() & 0o7777)
    };
    #[cfg(not(unix))]
    let perm_bits: i64 = if meta.is_dir() {
        0o755
    } else if meta.permissions().readonly() {
        0o444
    } else {
        0o644
    };
    let mode = kind_bits | perm_bits;
    d.insert(
        DictKey(Object::from_static("st_size")),
        Object::Int(meta.len() as i64),
    );
    d.insert(DictKey(Object::from_static("st_mode")), Object::Int(mode));
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
    d.insert(DictKey(Object::from_static("st_ino")), Object::Int(0));
    d.insert(DictKey(Object::from_static("st_dev")), Object::Int(0));
    d.insert(DictKey(Object::from_static("st_nlink")), Object::Int(1));
    d.insert(DictKey(Object::from_static("st_uid")), Object::Int(0));
    d.insert(DictKey(Object::from_static("st_gid")), Object::Int(0));
    drop(d);
    Object::Instance(Rc::new(inst))
}

fn os_readlink(args: &[Object]) -> Result<Object, RuntimeError> {
    let p = first_path(args, "readlink")?;
    let t = std::fs::read_link(&p).map_err(|e| crate::error::io_error_to_py(&e))?;
    Ok(Object::from_str(t.to_string_lossy().into_owned()))
}

fn os_chdir(args: &[Object]) -> Result<Object, RuntimeError> {
    let p = first_path(args, "chdir")?;
    std::env::set_current_dir(&p).map_err(|e| crate::error::io_error_to_py(&e))?;
    Ok(Object::None)
}

fn os_fspath(args: &[Object]) -> Result<Object, RuntimeError> {
    match args.first() {
        Some(Object::Str(_)) | Some(Object::Bytes(_)) => Ok(args[0].clone()),
        Some(other) => {
            // Best-effort: if it has __fspath__ we'd have invoked it
            // from the VM; here we just stringify.
            Ok(Object::from_str(format!("{:?}", other)))
        }
        None => Err(type_error("fspath() takes exactly one argument")),
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

fn os_walk(args: &[Object]) -> Result<Object, RuntimeError> {
    let p = first_path(args, "walk")?;
    let mut out = Vec::new();
    walk_dir(Path::new(&p), &mut out);
    Ok(Object::new_list(out))
}

fn walk_dir(root: &Path, out: &mut Vec<Object>) {
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    let entries = match std::fs::read_dir(root) {
        Ok(it) => it,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.file_type().map(|f| f.is_dir()).unwrap_or(false) {
            dirs.push(Object::from_str(name));
        } else {
            files.push(Object::from_str(name));
        }
    }
    let triple = Object::new_tuple(vec![
        Object::from_str(root.to_string_lossy().into_owned()),
        Object::new_list(dirs.clone()),
        Object::new_list(files),
    ]);
    out.push(triple);
    for d in dirs {
        if let Object::Str(name) = d {
            walk_dir(&root.join(name.as_ref()), out);
        }
    }
}

fn os_scandir(args: &[Object]) -> Result<Object, RuntimeError> {
    let p = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        None => ".".to_owned(),
        _ => return Err(type_error("scandir() arg must be str")),
    };
    let entries: Vec<Object> = std::fs::read_dir(&p)
        .map_err(|e| crate::error::io_error_to_py(&e))?
        .filter_map(|r| r.ok())
        .map(|entry| {
            let ft = entry.file_type().ok();
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path().to_string_lossy().into_owned();
            let is_dir = ft.as_ref().map(|f| f.is_dir()).unwrap_or(false);
            let is_file = ft.as_ref().map(|f| f.is_file()).unwrap_or(false);
            let is_symlink = ft.as_ref().map(|f| f.is_symlink()).unwrap_or(false);
            build_dir_entry(name, path, is_dir, is_file, is_symlink)
        })
        .collect();
    Ok(Object::new_list(entries))
}

/// Build a CPython-compatible ``os.DirEntry`` instance: attribute
/// access on ``name``/``path`` returns strings, while ``is_dir()``,
/// ``is_file()``, ``is_symlink()``, and ``stat()`` are zero-arg
/// methods so the standard ``e.is_file()`` form works.
fn build_dir_entry(
    name: String,
    path: String,
    is_dir: bool,
    is_file: bool,
    is_symlink: bool,
) -> Object {
    use crate::object::BuiltinFn;
    use crate::types::{PyInstance, TypeObject};
    thread_local! {
        static CLS: RefCell<Option<Rc<TypeObject>>> = const { RefCell::new(None) };
    }
    let class = CLS.with(|slot| {
        if let Some(c) = slot.borrow().as_ref() {
            return c.clone();
        }
        let bt = crate::builtin_types::builtin_types();
        let dict = DictData::new();
        let cls = TypeObject::new_user("DirEntry", vec![bt.object_.clone()], dict)
            .expect("DirEntry type");
        *slot.borrow_mut() = Some(cls.clone());
        cls
    });
    let inst = PyInstance::new(class);
    {
        let mut d = inst.dict.borrow_mut();
        d.insert(DictKey(Object::from_static("name")), Object::from_str(name));
        d.insert(
            DictKey(Object::from_static("path")),
            Object::from_str(path.clone()),
        );
        // Per-instance closures keep this DirEntry's own flag values
        // alive; a class-level method would otherwise leak the first
        // entry's flags to every subsequent one.
        let is_dir_v = is_dir;
        d.insert(
            DictKey(Object::from_static("is_dir")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "is_dir",
                call: Box::new(move |_args| Ok(Object::Bool(is_dir_v))),
                call_kw: None,
            })),
        );
        let is_file_v = is_file;
        d.insert(
            DictKey(Object::from_static("is_file")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "is_file",
                call: Box::new(move |_args| Ok(Object::Bool(is_file_v))),
                call_kw: None,
            })),
        );
        let is_symlink_v = is_symlink;
        d.insert(
            DictKey(Object::from_static("is_symlink")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "is_symlink",
                call: Box::new(move |_args| Ok(Object::Bool(is_symlink_v))),
                call_kw: None,
            })),
        );
        let path_for_stat = path;
        d.insert(
            DictKey(Object::from_static("stat")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "stat",
                call: Box::new(move |_args| {
                    let meta = std::fs::metadata(&path_for_stat)
                        .map_err(|e| crate::error::io_error_to_py(&e))?;
                    os_stat_to_result(&meta)
                }),
                call_kw: None,
            })),
        );
    }
    Object::Instance(Rc::new(inst))
}

/// Convert ``std::fs::Metadata`` to a ``SimpleNamespace`` shaped like
/// CPython's ``os.stat_result``. Used by ``DirEntry.stat()`` and a
/// future ``os.stat`` to match expectations from libraries that
/// rely on ``st_*`` attributes.
fn os_stat_to_result(meta: &std::fs::Metadata) -> Result<Object, RuntimeError> {
    let d = Rc::new(RefCell::new(DictData::new()));
    {
        let mut m = d.borrow_mut();
        m.insert(
            DictKey(Object::from_static("st_size")),
            Object::Int(meta.len() as i64),
        );
        m.insert(DictKey(Object::from_static("st_mode")), Object::Int(0o644));
        m.insert(
            DictKey(Object::from_static("st_mtime")),
            Object::Float(
                meta.modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs_f64())
                    .unwrap_or(0.0),
            ),
        );
    }
    Ok(Object::SimpleNamespace(d))
}

#[cfg(unix)]
fn os_kill(args: &[Object]) -> Result<Object, RuntimeError> {
    let pid = match args.first() {
        Some(Object::Int(p)) => *p as libc::pid_t,
        _ => return Err(type_error("kill() pid must be int")),
    };
    let sig = match args.get(1) {
        Some(Object::Int(s)) => *s as libc::c_int,
        _ => return Err(type_error("kill() signal must be int")),
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
    let rc = unsafe { libc::waitpid(pid, &raw mut status, options) };
    if rc < 0 {
        return Err(crate::error::io_error_to_py(
            &std::io::Error::last_os_error(),
        ));
    }
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

fn os_pipe(_args: &[Object]) -> Result<Object, RuntimeError> {
    #[cfg(unix)]
    {
        let mut fds = [0i32; 2];
        let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
        if rc != 0 {
            return Err(crate::error::os_error("pipe() failed"));
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

fn os_dup2(args: &[Object]) -> Result<Object, RuntimeError> {
    let fd = match args.first() {
        Some(Object::Int(i)) => *i as i32,
        _ => return Err(type_error("dup2() arg must be int")),
    };
    let newfd = match args.get(1) {
        Some(Object::Int(i)) => *i as i32,
        _ => return Err(type_error("dup2() arg2 must be int")),
    };
    #[cfg(unix)]
    {
        let new = unsafe { libc::dup2(fd, newfd) };
        if new < 0 {
            return Err(crate::error::os_error("dup2() failed"));
        }
        Ok(Object::Int(i64::from(new)))
    }
    #[cfg(not(unix))]
    {
        let _ = (fd, newfd);
        Err(crate::error::not_implemented_error(
            "os.dup2() is only implemented on POSIX in WeavePy",
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
        let r = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), n) };
        if r < 0 {
            return Err(crate::error::os_error("read() failed"));
        }
        buf.truncate(r as usize);
        Ok(Object::new_bytes(buf))
    }
    #[cfg(not(unix))]
    {
        let _ = (fd, n);
        Err(crate::error::not_implemented_error(
            "os.read() is only implemented on POSIX in WeavePy",
        ))
    }
}

fn os_write(args: &[Object]) -> Result<Object, RuntimeError> {
    let fd = match args.first() {
        Some(Object::Int(i)) => *i as i32,
        _ => return Err(type_error("write() arg must be int")),
    };
    let data = match args.get(1) {
        Some(Object::Bytes(b)) => b.to_vec(),
        Some(Object::ByteArray(b)) => b.borrow().clone(),
        Some(Object::Str(s)) => s.as_bytes().to_vec(),
        _ => return Err(type_error("write() arg2 must be bytes-like")),
    };
    #[cfg(unix)]
    {
        let r = unsafe { libc::write(fd, data.as_ptr().cast(), data.len()) };
        if r < 0 {
            return Err(crate::error::os_error("write() failed"));
        }
        Ok(Object::Int(r as i64))
    }
    #[cfg(not(unix))]
    {
        let _ = (fd, data);
        Err(crate::error::not_implemented_error(
            "os.write() is only implemented on POSIX in WeavePy",
        ))
    }
}

fn os_get_terminal_size(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::new_tuple(vec![Object::Int(80), Object::Int(24)]))
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

fn os_symlink(args: &[Object]) -> Result<Object, RuntimeError> {
    let src = first_path(args, "symlink")?;
    let dst = match args.get(1) {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Err(type_error("symlink() second arg must be str")),
    };
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
    let dst = match args.get(1) {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Err(type_error("link() second arg must be str")),
    };
    std::fs::hard_link(&src, &dst).map_err(|e| crate::error::io_error_to_py(&e))?;
    Ok(Object::None)
}

fn os_chmod(args: &[Object]) -> Result<Object, RuntimeError> {
    let p = first_path(args, "chmod")?;
    let mode = match args.get(1) {
        Some(Object::Int(m)) => *m as u32,
        _ => return Err(type_error("chmod() mode must be int")),
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&p)
            .map_err(|e| crate::error::io_error_to_py(&e))?
            .permissions();
        perms.set_mode(mode);
        std::fs::set_permissions(&p, perms).map_err(|e| crate::error::io_error_to_py(&e))?;
        Ok(Object::None)
    }
    #[cfg(not(unix))]
    {
        let _ = (p, mode);
        Ok(Object::None)
    }
}

fn os_utime(args: &[Object]) -> Result<Object, RuntimeError> {
    // Minimal implementation: just touch the file by opening for
    // append. A real version would call utimensat(2).
    let p = first_path(args, "utime")?;
    let _ = std::fs::OpenOptions::new()
        .write(true)
        .open(&p)
        .map_err(|e| crate::error::io_error_to_py(&e))?;
    Ok(Object::None)
}

fn path_like_type() -> Rc<crate::types::TypeObject> {
    path_like_type_singleton("PathLike")
}

fn path_like_type_singleton(name: &str) -> Rc<crate::types::TypeObject> {
    use crate::builtin_types::builtin_types;
    use crate::types::{TypeFlags, TypeObject};
    let bt = builtin_types();
    TypeObject::new_with_flags(
        Box::leak(name.to_owned().into_boxed_str()),
        vec![bt.object_.clone()],
        DictData::new(),
        TypeFlags {
            is_exception: false,
            is_builtin: true,
        },
    )
    .expect("os.PathLike")
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
        Some(obj) => as_str(obj, func),
        None => Err(type_error(format!("{func}() requires a path argument"))),
    }
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
