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

    let dict = Rc::new(RefCell::new(DictData::default()));
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
        // CPython 3.13 ships `os` frozen *with* a real `__file__`
        // (`.../os.py`); test_import's from-import error shapes embed
        // it (`cannot import name 'x' from 'os' (…/os.py)`). The
        // materialized tree carries a verbatim `os.py` for exactly
        // this identity (never executed — the built-in registry
        // resolves `os` first).
        if let Some(dir) = crate::stdlib_tree::stdlib_dir() {
            d.insert(
                DictKey(Object::from_static("__file__")),
                Object::from_str(dir.join("os.py").to_string_lossy().into_owned()),
            );
        }

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
        // environ block is bytes) and False on Windows, where the native
        // environment is UTF-16. Match it per platform — regrtest's
        // `setup_process` branches on this to pick `environb` vs `environ`.
        d.insert(
            DictKey(Object::from_static("supports_bytes_environ")),
            Object::Bool(cfg!(unix)),
        );
        d.insert(
            DictKey(Object::from_static("path")),
            Object::Module(path_mod),
        );
        d.insert(DictKey(Object::from_static("environ")), initial_environ());
        // `os.environb` (the bytes-keyed sibling) is installed by the
        // `_weave_envinit` frozen module, which upgrades both `environ` and
        // `environb` to CPython `_Environ` mappings over one shared store —
        // so no native `environb` default is needed here.

        d.insert(
            DictKey(Object::from_static("getcwd")),
            builtin("getcwd", os_getcwd),
        );
        // `os._get_exports_list(module)` — CPython's os.py helper; the
        // verbatim `socket.py` extends its `__all__` with it (RFC 0068 WS8).
        d.insert(
            DictKey(Object::from_static("_get_exports_list")),
            builtin("_get_exports_list", os_get_exports_list),
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
        // RFC 0040 WS1 — `os.sysconf(name)` + `os.sysconf_names`. asyncio's
        // `selector_events` probes `SC_IOV_MAX` the moment it sees
        // `socket.sendmsg`, and `concurrent.futures.ProcessPoolExecutor.
        // _check_system_limits` probes `SC_SEM_NSEMS_MAX`. Values are
        // platform-correct (the `_SC_*` ids differ between Linux and macOS).
        #[cfg(unix)]
        {
            d.insert(
                DictKey(Object::from_static("sysconf")),
                builtin("sysconf", os_sysconf),
            );
            d.insert(
                DictKey(Object::from_static("sysconf_names")),
                build_sysconf_names(),
            );
        }
        d.insert(
            DictKey(Object::from_static("remove")),
            builtin_kw("remove", os_remove_kw),
        );
        d.insert(
            DictKey(Object::from_static("unlink")),
            builtin_kw("unlink", os_remove_kw),
        );
        d.insert(
            DictKey(Object::from_static("mkdir")),
            builtin_kw("mkdir", os_mkdir_kw),
        );
        d.insert(
            DictKey(Object::from_static("makedirs")),
            builtin_kw("makedirs", os_makedirs_kw),
        );
        d.insert(
            DictKey(Object::from_static("rmdir")),
            builtin_kw("rmdir", os_rmdir_kw),
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
            builtin_kw("open", os_open_stub),
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
        // `os.openpty` is POSIX-only in CPython (no pty on NT); the name must
        // not exist on Windows so `hasattr` probes take the fallback branch.
        #[cfg(unix)]
        d.insert(
            DictKey(Object::from_static("openpty")),
            builtin("openpty", os_openpty),
        );
        #[cfg(unix)]
        d.insert(
            DictKey(Object::from_static("login_tty")),
            builtin("login_tty", os_login_tty),
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
            DictKey(Object::from_static("truncate")),
            builtin("truncate", os_truncate),
        );
        d.insert(
            DictKey(Object::from_static("times")),
            builtin("times", os_times),
        );
        d.insert(
            DictKey(Object::from_static("times_result")),
            Object::Type(times_result_type()),
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
            DictKey(Object::from_static("device_encoding")),
            builtin("device_encoding", os_device_encoding),
        );
        d.insert(
            DictKey(Object::from_static("read")),
            builtin("read", os_read),
        );
        d.insert(
            DictKey(Object::from_static("write")),
            builtin("write", os_write),
        );
        // CPython exposes `os.sendfile` on Linux/macOS/FreeBSD but never on
        // Windows, and `socket.py`/`asyncio` feature-detect it with
        // `hasattr(os, 'sendfile')` — gate the registration (like `uname`)
        // so the attribute is simply absent where the syscall is.
        #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
        d.insert(
            DictKey(Object::from_static("sendfile")),
            builtin_kw("sendfile", os_sendfile),
        );
        d.insert(
            DictKey(Object::from_static("get_terminal_size")),
            builtin("get_terminal_size", os_get_terminal_size),
        );
        // CPython only exposes `os.uname`/`os.uname_result` on POSIX. Code in
        // the wild feature-detects with `hasattr(os, 'uname')` (e.g.
        // `test.support` does `hasattr(os, 'uname') and os.uname()...`), so
        // registering a stub that *raises* on Windows would make `hasattr`
        // report `True` and then blow up on the call. Gate the registration to
        // Unix so the attribute is simply absent on Windows, matching CPython.
        #[cfg(unix)]
        {
            d.insert(
                DictKey(Object::from_static("uname")),
                builtin("uname", os_uname),
            );
            d.insert(
                DictKey(Object::from_static("uname_result")),
                Object::Type(uname_result_type()),
            );
        }
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
            DictKey(Object::from_static("system")),
            builtin("system", os_system),
        );
        d.insert(
            DictKey(Object::from_static("waitstatus_to_exitcode")),
            builtin("waitstatus_to_exitcode", os_waitstatus_to_exitcode),
        );
        // `os.get_blocking`/`os.set_blocking` are Unix-only in CPython
        // (`O_NONBLOCK` has no CRT-fd analogue); asyncio's proactor path never
        // calls them on Windows, and their absence is the documented signal.
        #[cfg(unix)]
        {
            d.insert(
                DictKey(Object::from_static("set_blocking")),
                builtin("set_blocking", os_set_blocking),
            );
            d.insert(
                DictKey(Object::from_static("get_blocking")),
                builtin("get_blocking", os_get_blocking),
            );
        }
        // Common signal numbers — match libc on POSIX. CPython's `os` never
        // exports `SIG*` (they live in `signal`) nor `WNOHANG` on Windows, so
        // these WeavePy conveniences stay Unix-only.
        #[cfg(unix)]
        {
            d.insert(DictKey(Object::from_static("SIGTERM")), Object::Int(15));
            d.insert(DictKey(Object::from_static("SIGKILL")), Object::Int(9));
            d.insert(DictKey(Object::from_static("SIGINT")), Object::Int(2));
            d.insert(DictKey(Object::from_static("SIGHUP")), Object::Int(1));
            d.insert(DictKey(Object::from_static("WNOHANG")), Object::Int(1));
        }

        // RFC 0040 WS1: POSIX process & fd primitives (fork/exec*/
        // posix_spawn/wait*/W*/closerange/setsid/register_at_fork/…).
        crate::stdlib::os_process::register(&mut d);
        // Safety net for entry points that don't snapshot the OS-thread
        // baseline at startup (embedders, the in-process conformance runner):
        // capture it on first `os` import if it's still unset. Never clobbers
        // the CLI's authoritative early capture.
        crate::stdlib::os_process::capture_thread_baseline_if_unset();
        d.insert(
            DictKey(Object::from_static("get_exec_path")),
            builtin("get_exec_path", os_get_exec_path),
        );
        // uid/gid getters are POSIX-only surface: CPython's `nt` module has no
        // `getuid` (code probes `hasattr(os, 'getuid')` to detect Unix).
        #[cfg(unix)]
        {
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
        }
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
        // `os.fchmod` is Unix-only in CPython (`HAVE_FCHMOD`); on Windows even
        // `os.chmod(fd, …)` is a TypeError (the path converter rejects fds).
        #[cfg(unix)]
        d.insert(
            DictKey(Object::from_static("fchmod")),
            builtin("fchmod", os_fchmod),
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
        // File-open flag bits. On POSIX these are sourced from `libc` so each
        // constant equals the *host* platform's real `O_*` value: CPython
        // exposes the native values, and `os.pipe2`/`os.open`/`fcntl` pass them
        // straight to the kernel. On Linux they match the historical hard-coded
        // numbers; on macOS several differ — `O_NONBLOCK` is `0x4` (not the
        // Linux `0x800`), and `O_TRUNC`/`O_APPEND`/`O_CREAT`/`O_EXCL` likewise.
        // The old Linux-valued constants made `os.pipe2(os.O_NONBLOCK)` a no-op
        // on macOS (`flags & libc::O_NONBLOCK == 0`), so the pipe stayed
        // blocking (`test_posix.test_pipe2`). O_RDONLY/WRONLY/RDWR are 0/1/2 on
        // every platform.
        #[cfg(unix)]
        {
            d.insert(
                DictKey(Object::from_static("O_RDONLY")),
                Object::Int(i64::from(libc::O_RDONLY)),
            );
            d.insert(
                DictKey(Object::from_static("O_WRONLY")),
                Object::Int(i64::from(libc::O_WRONLY)),
            );
            d.insert(
                DictKey(Object::from_static("O_RDWR")),
                Object::Int(i64::from(libc::O_RDWR)),
            );
            d.insert(
                DictKey(Object::from_static("O_CREAT")),
                Object::Int(i64::from(libc::O_CREAT)),
            );
            d.insert(
                DictKey(Object::from_static("O_EXCL")),
                Object::Int(i64::from(libc::O_EXCL)),
            );
            d.insert(
                DictKey(Object::from_static("O_TRUNC")),
                Object::Int(i64::from(libc::O_TRUNC)),
            );
            d.insert(
                DictKey(Object::from_static("O_APPEND")),
                Object::Int(i64::from(libc::O_APPEND)),
            );
            d.insert(
                DictKey(Object::from_static("O_NONBLOCK")),
                Object::Int(i64::from(libc::O_NONBLOCK)),
            );
            d.insert(
                DictKey(Object::from_static("O_NDELAY")),
                Object::Int(i64::from(libc::O_NDELAY)),
            );
            d.insert(
                DictKey(Object::from_static("O_SYNC")),
                Object::Int(i64::from(libc::O_SYNC)),
            );
            d.insert(
                DictKey(Object::from_static("O_NOCTTY")),
                Object::Int(i64::from(libc::O_NOCTTY)),
            );
            d.insert(
                DictKey(Object::from_static("O_ACCMODE")),
                Object::Int(i64::from(libc::O_ACCMODE)),
            );
        }
        // Windows: the CRT's `_open`/`_wsopen_s` flag values (fcntl.h), which
        // differ from every POSIX platform's. CPython's `nt` publishes exactly
        // this set (posixmodule.c `all_ins`): the shared O_* core plus the
        // CRT-only text/binary/inheritance/lifetime bits. There is no
        // `O_NONBLOCK`/`O_CLOEXEC`/`O_NOCTTY` on Windows.
        #[cfg(windows)]
        {
            use crate::stdlib::nt_support::crt;
            for (name, v) in [
                ("O_RDONLY", crt::O_RDONLY),
                ("O_WRONLY", crt::O_WRONLY),
                ("O_RDWR", crt::O_RDWR),
                ("O_CREAT", crt::O_CREAT),
                ("O_EXCL", crt::O_EXCL),
                ("O_TRUNC", crt::O_TRUNC),
                ("O_APPEND", crt::O_APPEND),
                ("O_TEXT", crt::O_TEXT),
                ("O_BINARY", crt::O_BINARY),
                ("O_NOINHERIT", crt::O_NOINHERIT),
                ("O_TEMPORARY", crt::O_TEMPORARY),
                ("O_SHORT_LIVED", crt::O_SHORT_LIVED),
                ("O_RANDOM", crt::O_RANDOM),
                ("O_SEQUENTIAL", crt::O_SEQUENTIAL),
            ] {
                d.insert(
                    DictKey(Object::from_static(name)),
                    Object::Int(i64::from(v)),
                );
            }
        }
        #[cfg(not(any(unix, windows)))]
        {
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
        }
        // `O_CLOEXEC` is platform-specific (and `O_DIRECT` is Linux-only), so
        // source them from `libc` — `os.pipe2`/`os.open` callers and
        // `test_posix.test_pipe2` expect them present.
        #[cfg(unix)]
        d.insert(
            DictKey(Object::from_static("O_CLOEXEC")),
            Object::Int(i64::from(libc::O_CLOEXEC)),
        );
        // `O_DIRECTORY` (open only if the target is a directory) and
        // `O_NOFOLLOW` (fail on a trailing symlink) exist on Linux and the
        // BSDs/macOS. `test_glob` opens a `dir_fd` with
        // `os.open(dir, O_RDONLY | O_DIRECTORY)` in `setUp`, so their absence
        // turned every dir_fd-based glob test into an `AttributeError`.
        #[cfg(unix)]
        d.insert(
            DictKey(Object::from_static("O_DIRECTORY")),
            Object::Int(i64::from(libc::O_DIRECTORY)),
        );
        #[cfg(unix)]
        d.insert(
            DictKey(Object::from_static("O_NOFOLLOW")),
            Object::Int(i64::from(libc::O_NOFOLLOW)),
        );
        #[cfg(target_os = "linux")]
        d.insert(
            DictKey(Object::from_static("O_DIRECT")),
            Object::Int(i64::from(libc::O_DIRECT)),
        );
        // `lseek` whence values — identical across every POSIX platform.
        d.insert(DictKey(Object::from_static("SEEK_SET")), Object::Int(0));
        d.insert(DictKey(Object::from_static("SEEK_CUR")), Object::Int(1));
        d.insert(DictKey(Object::from_static("SEEK_END")), Object::Int(2));
        d.insert(DictKey(Object::from_static("F_OK")), Object::Int(0));
        d.insert(DictKey(Object::from_static("R_OK")), Object::Int(4));
        d.insert(DictKey(Object::from_static("W_OK")), Object::Int(2));
        d.insert(DictKey(Object::from_static("X_OK")), Object::Int(1));
        // `EX_*` come from `<sysexits.h>`, which Windows lacks — CPython only
        // exposes them where the header defines them, so gate to Unix.
        #[cfg(unix)]
        {
            d.insert(DictKey(Object::from_static("EX_OK")), Object::Int(0));
            d.insert(DictKey(Object::from_static("EX_USAGE")), Object::Int(64));
            d.insert(DictKey(Object::from_static("EX_DATAERR")), Object::Int(65));
            d.insert(DictKey(Object::from_static("EX_NOINPUT")), Object::Int(66));
            d.insert(DictKey(Object::from_static("EX_SOFTWARE")), Object::Int(70));
            d.insert(DictKey(Object::from_static("EX_OSERR")), Object::Int(71));
            d.insert(DictKey(Object::from_static("EX_IOERR")), Object::Int(74));
        }
        // macOS `fcopyfile(3)` fast clone. CPython exposes `posix._fcopyfile`
        // plus the `_COPYFILE_*` flag bits; `shutil.copyfile` uses them for a
        // zero-copy reflink on APFS/HFS+ (`test_shutil.TestZeroCopyMACOS`, and
        // the `_HAS_FCOPYFILE` fast path in the bundled `shutil`).
        #[cfg(target_os = "macos")]
        {
            d.insert(
                DictKey(Object::from_static("_fcopyfile")),
                builtin("_fcopyfile", os_fcopyfile),
            );
            d.insert(
                DictKey(Object::from_static("_COPYFILE_ACL")),
                Object::Int(1),
            );
            d.insert(
                DictKey(Object::from_static("_COPYFILE_STAT")),
                Object::Int(2),
            );
            d.insert(
                DictKey(Object::from_static("_COPYFILE_XATTR")),
                Object::Int(4),
            );
            d.insert(
                DictKey(Object::from_static("_COPYFILE_DATA")),
                Object::Int(8),
            );
        }

        // RFC 0040 WS1 — `os.pathconf(path, name)` / `os.fpathconf(fd, name)`
        // and the `os.pathconf_names` mapping. `tarfile`'s
        // `test_realpath_limit_attack` (CVE-2025-4517 regression) sizes its
        // near-`PATH_MAX` symlink tree via `os.pathconf(parent, "PC_PATH_MAX")`.
        #[cfg(unix)]
        {
            d.insert(
                DictKey(Object::from_static("pathconf")),
                builtin("pathconf", os_pathconf),
            );
            d.insert(
                DictKey(Object::from_static("fpathconf")),
                builtin("fpathconf", os_fpathconf),
            );
            d.insert(
                DictKey(Object::from_static("pathconf_names")),
                build_pathconf_names(),
            );
        }

        // RFC 0063 WS1 — the NT-only surface CPython's `nt` module exports on
        // Windows (posixmodule.c under `MS_WINDOWS`). Portable code probes
        // these with `hasattr` (`shutil.disk_usage`, `webbrowser`,
        // `getpass.getuser`), and `ntpath` imports the `_get*` fast paths.
        #[cfg(windows)]
        {
            d.insert(
                DictKey(Object::from_static("getlogin")),
                builtin("getlogin", os_getlogin),
            );
            d.insert(
                DictKey(Object::from_static("startfile")),
                builtin_kw("startfile", os_startfile),
            );
            // `os.fsync` (CRT `_commit`). Registered Windows-only for now: the
            // POSIX build never exposed `fsync`, and adding it there would
            // change the measured host surface outside this wave's scope.
            d.insert(
                DictKey(Object::from_static("fsync")),
                builtin("fsync", os_fsync),
            );
            d.insert(
                DictKey(Object::from_static("_getfullpathname")),
                builtin("_getfullpathname", nt_getfullpathname),
            );
            d.insert(
                DictKey(Object::from_static("_getfinalpathname")),
                builtin("_getfinalpathname", nt_getfinalpathname),
            );
            d.insert(
                DictKey(Object::from_static("_getvolumepathname")),
                builtin("_getvolumepathname", nt_getvolumepathname),
            );
            d.insert(
                DictKey(Object::from_static("_getdiskusage")),
                builtin("_getdiskusage", nt_getdiskusage),
            );
            d.insert(
                DictKey(Object::from_static("_path_splitroot_ex")),
                builtin("_path_splitroot_ex", nt_path_splitroot_ex),
            );
            // RFC 0064 WS2 — `os.add_dll_directory` (PEP 578-audited
            // `AddDllDirectory`). Binary wheels' `__init__` shims call it
            // to make vendored dependent DLLs resolvable by the loader
            // flags the extension loader passes (`LOAD_LIBRARY_SEARCH_
            // DEFAULT_DIRS` honours these cookies; `PATH`/CWD are not
            // searched — bpo-36085).
            d.insert(
                DictKey(Object::from_static("add_dll_directory")),
                builtin_kw("add_dll_directory", os_add_dll_directory),
            );
        }

        // `os.supports_follow_symlinks` must hold the *function objects* that
        // honour `follow_symlinks=` — `shutil.copystat`/`copy2` and `tempfile`
        // test membership (`fn in os.supports_follow_symlinks`) and fall back to
        // a no-op (returning `None`) otherwise. WeavePy's `stat`/`chmod`/`utime`
        // all thread the keyword through to `*at(AT_SYMLINK_NOFOLLOW)`, so
        // advertise exactly those (CPython lists more, but we only claim what we
        // faithfully implement).
        let follow_objs: Vec<Object> = ["stat", "chmod", "utime"]
            .iter()
            .filter_map(|n| d.get(&DictKey(Object::from_static(n))).cloned())
            .collect();
        d.insert(
            DictKey(Object::from_static("supports_follow_symlinks")),
            Object::new_set_from(follow_objs),
        );
        // RFC 0040 WS1 — advertise the functions whose `dir_fd=`/`fd` keywords
        // WeavePy genuinely honours (via `*at(2)`/`fdopendir`). `shutil.rmtree`
        // gates its hardened, symlink-race-free `_rmtree_safe_fd` path on
        // `{open, stat, unlink, rmdir} <= os.supports_dir_fd` *and*
        // `os.scandir in os.supports_fd`, so membership must hold the very same
        // function objects (set membership is by identity). `tarfile`'s
        // `test_realpath_limit_attack` cleanup deletes a near-`PATH_MAX` tree,
        // which only the fd path can do without `ENAMETOOLONG`.
        #[cfg(unix)]
        {
            let dir_fd_objs: Vec<Object> = [
                "open", "stat", "lstat", "unlink", "remove", "rmdir", "mkdir",
            ]
            .iter()
            .filter_map(|n| d.get(&DictKey(Object::from_static(n))).cloned())
            .collect();
            d.insert(
                DictKey(Object::from_static("supports_dir_fd")),
                Object::new_set_from(dir_fd_objs),
            );
            let fd_objs: Vec<Object> = ["scandir", "listdir"]
                .iter()
                .filter_map(|n| d.get(&DictKey(Object::from_static(n))).cloned())
                .collect();
            d.insert(
                DictKey(Object::from_static("supports_fd")),
                Object::new_set_from(fd_objs),
            );
        }
    }
    Rc::new(PyModule {
        name: "os".to_owned(),
        filename: None,
        dict,
    })
}

pub fn build_path(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::default()));
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

/// Reject any positional arguments. CPython's argument-clinic-generated
/// no-arg syscalls (`os.getpid`, `os.getuid`, `os.uname`, …) raise
/// `TypeError` when handed an argument (`test_posix.testNoArgFunctions`
/// asserts this for the whole family); WeavePy's native bodies otherwise
/// silently ignore extras, so gate them through this helper.
pub(super) fn require_no_args(args: &[Object], name: &str) -> Result<(), RuntimeError> {
    if !args.is_empty() {
        return Err(crate::error::type_error(format!(
            "{name}() takes no arguments ({} given)",
            args.len()
        )));
    }
    Ok(())
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
#[cfg_attr(not(unix), allow(dead_code))]
pub(super) fn sequence_items(o: &Object) -> Option<Vec<Object>> {
    match o {
        Object::Tuple(t) => Some(t.to_vec()),
        Object::List(l) => Some(l.borrow().clone()),
        Object::Set(s) => Some(s.borrow().iter().map(|k| k.0.clone()).collect()),
        Object::FrozenSet(s) => Some(s.iter().map(|k| k.0.clone()).collect()),
        _ => None,
    }
}

/// Decode an OS string (env var, in PEP 383 terms) to a `str`/`WStr` using the
/// filesystem codec (UTF-8) + `surrogateescape`, so an undecodable byte
/// (0x80..0xFF) becomes a lone surrogate (U+DC80..U+DCFF) that `_weave_envinit`
/// can re-encode back to the exact original byte. `std::env::vars()` would
/// instead *panic* on a non-UTF-8 value, so the `_os`-level snapshot must go
/// through the byte-faithful `*_os` APIs.
fn fsdecode_osstr(s: &std::ffi::OsStr) -> Object {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        crate::stdlib::codecs_mod::decode_bytes_obj(s.as_bytes(), "utf-8", "surrogateescape")
            .unwrap_or_else(|_| Object::from_str(s.to_string_lossy().into_owned()))
    }
    #[cfg(not(unix))]
    {
        Object::from_str(s.to_string_lossy().into_owned())
    }
}

fn initial_environ() -> Object {
    let mut d = DictData::default();
    // `vars_os` (not `vars`) so an undecodable env value doesn't panic; each
    // entry is fsdecoded with `surrogateescape` (PEP 383) for a faithful
    // round-trip through `os.environ` / `os.environb`
    // (test_subprocess.test_undecodable_env).
    for (k, v) in std::env::vars_os() {
        // Windows environment names are case-insensitive; CPython's `os.py`
        // normalises them by wrapping `nt.environ` in an `_Environ` whose
        // `encodekey` is `str.upper`, so every visible key is upper-cased at
        // snapshot time. Match that here since WeavePy's `os` is native.
        #[cfg(windows)]
        let key = Object::from_str(k.to_string_lossy().to_uppercase());
        #[cfg(not(windows))]
        let key = fsdecode_osstr(&k);
        d.insert(DictKey(key), fsdecode_osstr(&v));
    }
    Object::Dict(Rc::new(RefCell::new(d)))
}

fn os_getcwd(args: &[Object]) -> Result<Object, RuntimeError> {
    require_no_args(args, "getcwd")?;
    let cwd = std::env::current_dir().map_err(getcwd_error)?;
    Ok(Object::from_str(cwd.to_string_lossy().into_owned()))
}

/// CPython's `os._get_exports_list(module)`: `list(module.__all__)`, or
/// the non-underscore names from the module dict when `__all__` is absent.
fn os_get_exports_list(args: &[Object]) -> Result<Object, RuntimeError> {
    let Some(Object::Module(m)) = args.first() else {
        return Err(crate::error::type_error(
            "_get_exports_list() argument must be a module",
        ));
    };
    let dict = m.dict.borrow();
    if let Some(all) = dict.get(&DictKey(Object::from_static("__all__"))) {
        if let Object::List(items) = all {
            return Ok(Object::new_list(items.borrow().clone()));
        }
        if let Object::Tuple(items) = all {
            return Ok(Object::new_list(items.to_vec()));
        }
    }
    let mut names: Vec<Object> = dict
        .keys()
        .filter_map(|k| match &k.0 {
            Object::Str(s) if !s.starts_with('_') => Some(k.0.clone()),
            _ => None,
        })
        .collect();
    names.sort_by_key(|o| o.to_str());
    Ok(Object::new_list(names))
}

/// A deleted working directory must surface as `FileNotFoundError`, not a
/// bare `OSError` — `_bootstrap_external._path_importer_cache` catches
/// exactly `(FileNotFoundError, PermissionError)` around `_os.getcwd()`
/// (import_.test_path test_deleted_cwd).
fn getcwd_error(e: std::io::Error) -> RuntimeError {
    let errno = e.raw_os_error().unwrap_or(0);
    let class = match e.kind() {
        std::io::ErrorKind::NotFound => "FileNotFoundError",
        std::io::ErrorKind::PermissionDenied => "PermissionError",
        _ => "OSError",
    };
    crate::error::oserror_subclass_with_errno(class, errno, format!("getcwd: {e}"))
}

/// `os.getcwdb()` — the working directory as `bytes` (the OS-encoded path).
/// `posixpath.abspath`/`realpath` call this for bytes-typed inputs.
fn os_getcwdb(args: &[Object]) -> Result<Object, RuntimeError> {
    require_no_args(args, "getcwdb")?;
    let cwd = std::env::current_dir().map_err(getcwd_error)?;
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

/// Windows flavour of [`env_cstring`]: coerce to a Rust `String` for
/// `std::env`, raising `ValueError` on an embedded NUL like CPython.
/// Bytes are decoded as UTF-8 (WeavePy's filesystem encoding).
#[cfg(not(unix))]
fn env_string(o: Option<&Object>, what: &str) -> Result<String, RuntimeError> {
    let bytes = match o {
        Some(Object::Str(s)) => s.as_bytes().to_vec(),
        Some(Object::Bytes(b)) => b.to_vec(),
        Some(Object::ByteArray(b)) => b.borrow().clone(),
        _ => return Err(type_error(format!("putenv() {what} must be str or bytes"))),
    };
    if bytes.contains(&0) {
        return Err(crate::error::value_error("embedded null byte"));
    }
    String::from_utf8(bytes)
        .map_err(|_| crate::error::value_error(format!("putenv() {what} is not valid UTF-8")))
}

fn os_putenv(args: &[Object]) -> Result<Object, RuntimeError> {
    #[cfg(unix)]
    {
        let name = env_cstring(args.first(), "name")?;
        // An `=` in the *name* is illegal (it would split the `NAME=VALUE`
        // record); CPython raises `ValueError` rather than letting `setenv`
        // fail with `EINVAL` (`test_posix.test_putenv`).
        if name.as_bytes().contains(&b'=') {
            return Err(crate::error::value_error(
                "illegal environment variable name",
            ));
        }
        let value = env_cstring(args.get(1), "value")?;
        // setenv (overwrite=1) edits the live C environ, so a later `execv`
        // (which passes the inherited environ) carries the change into the
        // child — exactly what `os.putenv` promises.
        if unsafe { libc::setenv(name.as_ptr(), value.as_ptr(), 1) } != 0 {
            return Err(crate::error::io_error_to_py(
                &std::io::Error::last_os_error(),
            ));
        }
        Ok(Object::None)
    }
    #[cfg(not(unix))]
    {
        // Windows: CPython implements putenv via the CRT's `_wputenv`;
        // `std::env::set_var` (SetEnvironmentVariableW) equally edits the
        // live process environment so children inherit the change. The
        // validation mirrors the POSIX branch — and keeps `set_var` from
        // panicking on an illegal name.
        let name = env_string(args.first(), "name")?;
        if name.is_empty() || name.contains('=') {
            return Err(crate::error::value_error(
                "illegal environment variable name",
            ));
        }
        let value = env_string(args.get(1), "value")?;
        std::env::set_var(name, value);
        Ok(Object::None)
    }
}

fn os_unsetenv(args: &[Object]) -> Result<Object, RuntimeError> {
    #[cfg(unix)]
    {
        let name = env_cstring(args.first(), "name")?;
        if unsafe { libc::unsetenv(name.as_ptr()) } != 0 {
            return Err(crate::error::io_error_to_py(
                &std::io::Error::last_os_error(),
            ));
        }
        Ok(Object::None)
    }
    #[cfg(not(unix))]
    {
        let name = env_string(args.first(), "name")?;
        if name.is_empty() || name.contains('=') {
            return Err(crate::error::value_error(
                "illegal environment variable name",
            ));
        }
        std::env::remove_var(name);
        Ok(Object::None)
    }
}

fn os_getpid(args: &[Object]) -> Result<Object, RuntimeError> {
    require_no_args(args, "getpid")?;
    Ok(Object::Int(i64::from(std::process::id())))
}

/// The `SC_*` names WeavePy maps to libc `_SC_*` ids. The id values differ
/// per platform, so we let libc supply them (e.g. `_SC_IOV_MAX` is 56 on
/// macOS but 60 on Linux). This subset covers the names CPython's stdlib
/// (asyncio, multiprocessing) and common scripts query.
#[cfg(unix)]
fn sysconf_name_table() -> &'static [(&'static str, libc::c_int)] {
    &[
        ("SC_ARG_MAX", libc::_SC_ARG_MAX),
        ("SC_CHILD_MAX", libc::_SC_CHILD_MAX),
        ("SC_CLK_TCK", libc::_SC_CLK_TCK),
        ("SC_NGROUPS_MAX", libc::_SC_NGROUPS_MAX),
        ("SC_OPEN_MAX", libc::_SC_OPEN_MAX),
        ("SC_STREAM_MAX", libc::_SC_STREAM_MAX),
        ("SC_TZNAME_MAX", libc::_SC_TZNAME_MAX),
        ("SC_JOB_CONTROL", libc::_SC_JOB_CONTROL),
        ("SC_SAVED_IDS", libc::_SC_SAVED_IDS),
        ("SC_VERSION", libc::_SC_VERSION),
        ("SC_PAGESIZE", libc::_SC_PAGESIZE),
        ("SC_PAGE_SIZE", libc::_SC_PAGESIZE),
        ("SC_LINE_MAX", libc::_SC_LINE_MAX),
        ("SC_HOST_NAME_MAX", libc::_SC_HOST_NAME_MAX),
        ("SC_LOGIN_NAME_MAX", libc::_SC_LOGIN_NAME_MAX),
        ("SC_TTY_NAME_MAX", libc::_SC_TTY_NAME_MAX),
        ("SC_NPROCESSORS_CONF", libc::_SC_NPROCESSORS_CONF),
        ("SC_NPROCESSORS_ONLN", libc::_SC_NPROCESSORS_ONLN),
        ("SC_PHYS_PAGES", libc::_SC_PHYS_PAGES),
        ("SC_IOV_MAX", libc::_SC_IOV_MAX),
        ("SC_SEM_NSEMS_MAX", libc::_SC_SEM_NSEMS_MAX),
        ("SC_SEM_VALUE_MAX", libc::_SC_SEM_VALUE_MAX),
        ("SC_AIO_MAX", libc::_SC_AIO_MAX),
        ("SC_THREAD_THREADS_MAX", libc::_SC_THREAD_THREADS_MAX),
    ]
}

/// `os.sysconf_names` — the `{name: id}` mapping CPython exposes.
#[cfg(unix)]
fn build_sysconf_names() -> Object {
    let mut d = DictData::default();
    for (name, id) in sysconf_name_table() {
        d.insert(
            DictKey(Object::from_static(name)),
            Object::Int(i64::from(*id)),
        );
    }
    Object::Dict(Rc::new(RefCell::new(d)))
}

#[cfg(unix)]
fn sysconf_name_to_id(name: &str) -> Option<libc::c_int> {
    sysconf_name_table()
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, id)| *id)
}

/// errno is thread-local; the accessor symbol differs across platforms.
#[cfg(any(target_os = "linux", target_os = "android"))]
unsafe fn errno_location() -> *mut libc::c_int {
    unsafe { libc::__errno_location() }
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
unsafe fn errno_location() -> *mut libc::c_int {
    unsafe { libc::__error() }
}

/// `os.sysconf(name)` — query a runtime system limit. `name` is either a
/// key of `os.sysconf_names` or a raw integer id. Mirrors CPython: a `-1`
/// return with a clean errno means "indeterminate/unlimited" and is returned
/// as `-1`; a `-1` with errno set raises `OSError`.
#[cfg(unix)]
fn os_sysconf(args: &[Object]) -> Result<Object, RuntimeError> {
    let id: libc::c_int = match args.first() {
        Some(Object::Int(n)) => *n as libc::c_int,
        Some(Object::Str(s)) => {
            sysconf_name_to_id(s).ok_or_else(|| value_error("unrecognized configuration name"))?
        }
        _ => {
            return Err(type_error(
                "configuration names must be strings or integers",
            ))
        }
    };
    // SAFETY: errno is a valid thread-local int; `sysconf` is async-signal-safe
    // and only reads `id`.
    unsafe {
        *errno_location() = 0;
    }
    let val = unsafe { libc::sysconf(id) };
    if val == -1 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error().unwrap_or(0) != 0 {
            return Err(crate::error::io_error_to_py(&err));
        }
    }
    Ok(Object::Int(val as i64))
}

/// The POSIX `pathconf`/`fpathconf` name → `_PC_*` id table. Only the
/// portable POSIX.1 set is exposed (identical ids would differ per platform,
/// so each maps through the `libc` constant). `PC_PATH_MAX` is the one
/// `tarfile`'s `test_realpath_limit_attack` (CVE-2025-4517 regression) needs
/// to size its near-`PATH_MAX` symlink tree.
#[cfg(unix)]
fn pathconf_name_table() -> &'static [(&'static str, libc::c_int)] {
    &[
        ("PC_LINK_MAX", libc::_PC_LINK_MAX),
        ("PC_MAX_CANON", libc::_PC_MAX_CANON),
        ("PC_MAX_INPUT", libc::_PC_MAX_INPUT),
        ("PC_NAME_MAX", libc::_PC_NAME_MAX),
        ("PC_PATH_MAX", libc::_PC_PATH_MAX),
        ("PC_PIPE_BUF", libc::_PC_PIPE_BUF),
        ("PC_CHOWN_RESTRICTED", libc::_PC_CHOWN_RESTRICTED),
        ("PC_NO_TRUNC", libc::_PC_NO_TRUNC),
        ("PC_VDISABLE", libc::_PC_VDISABLE),
    ]
}

/// `os.pathconf_names` — the `{name: id}` mapping CPython exposes.
#[cfg(unix)]
fn build_pathconf_names() -> Object {
    let mut d = DictData::default();
    for (name, id) in pathconf_name_table() {
        d.insert(
            DictKey(Object::from_static(name)),
            Object::Int(i64::from(*id)),
        );
    }
    Object::Dict(Rc::new(RefCell::new(d)))
}

#[cfg(unix)]
fn pathconf_name_to_id(name: &str) -> Option<libc::c_int> {
    pathconf_name_table()
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, id)| *id)
}

/// Resolve a `pathconf`/`fpathconf` configuration name argument (a key of
/// `os.pathconf_names` or a raw integer id) to its `_PC_*` id, matching
/// CPython's `conv_confname` error messages.
#[cfg(unix)]
fn pathconf_arg_id(arg: Option<&Object>) -> Result<libc::c_int, RuntimeError> {
    match arg {
        Some(Object::Int(n)) => Ok(*n as libc::c_int),
        Some(Object::Str(s)) => {
            pathconf_name_to_id(s).ok_or_else(|| value_error("unrecognized configuration name"))
        }
        _ => Err(type_error(
            "configuration names must be strings or integers",
        )),
    }
}

/// `os.pathconf(path, name)` — query a path-scoped POSIX limit. As with
/// `sysconf`, a `-1` return with a clean errno means "indeterminate/unlimited"
/// and is returned as-is; a `-1` with errno set raises `OSError`. CPython's
/// `path_t(allow_fd=True)` also accepts an integer descriptor, transparently
/// using `fpathconf` semantics (`test_os.TestInvalidFD`).
#[cfg(unix)]
fn os_pathconf(args: &[Object]) -> Result<Object, RuntimeError> {
    // `os.pathconf(fd, name)` with an int path delegates to `fpathconf`.
    if matches!(args.first(), Some(Object::Int(_) | Object::Bool(_))) {
        return os_fpathconf(args);
    }
    let path = first_path(args, "pathconf")?;
    let id = pathconf_arg_id(args.get(1))?;
    let cpath =
        std::ffi::CString::new(path.as_bytes()).map_err(|_| value_error("embedded null byte"))?;
    // SAFETY: errno is a valid thread-local int; `pathconf` only reads the
    // (NUL-terminated) path and id.
    unsafe {
        *errno_location() = 0;
    }
    let val = unsafe { libc::pathconf(cpath.as_ptr(), id) };
    if val == -1 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error().unwrap_or(0) != 0 {
            return Err(path_io_err(&err, args.first(), &path));
        }
    }
    Ok(Object::Int(val as i64))
}

/// `os.fpathconf(fd, name)` — the descriptor-relative counterpart of
/// [`os_pathconf`].
#[cfg(unix)]
fn os_fpathconf(args: &[Object]) -> Result<Object, RuntimeError> {
    let fd = match args.first() {
        // A `bool` descriptor warns ("bool is used as a file descriptor"),
        // matching CPython's `_PyLong_FileDescriptor_Converter`
        // (`test_os.TestInvalidFD.check_bool`).
        Some(Object::Bool(b)) => {
            warn_bool_as_fd()?;
            libc::c_int::from(*b)
        }
        Some(Object::Int(n)) => *n as libc::c_int,
        _ => return Err(type_error("fpathconf() argument 'fd' must be an int")),
    };
    let id = pathconf_arg_id(args.get(1))?;
    unsafe {
        *errno_location() = 0;
    }
    let val = unsafe { libc::fpathconf(fd, id) };
    if val == -1 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error().unwrap_or(0) != 0 {
            return Err(crate::error::io_error_to_py(&err));
        }
    }
    Ok(Object::Int(val as i64))
}

/// Build an `OSError` for a failed single-path syscall, preserving the
/// *identity* of the caller's original path object as `.filename` when one was
/// passed positionally (`test_os.test_oserror_filename` asserts
/// `err.filename is name`, even for a `str` subclass or `bytes`). `display` is
/// the textual path used only in the `[Errno N] strerror: 'name'` message, so
/// the rendered text is unchanged from the string-path helper.
fn path_io_err(e: &std::io::Error, path_obj: Option<&Object>, display: &str) -> RuntimeError {
    match path_obj {
        Some(o) => crate::error::io_error_to_py_path(e, o, display),
        None => crate::error::io_error_to_py_named(e, Some(display)),
    }
}

/// Two-path counterpart of [`path_io_err`] for `rename`/`replace`/`link`,
/// keeping the identity of the *first* path object as `.filename` (the one
/// CPython's `test_oserror_filename` checks).
fn path_io_err2(
    e: &std::io::Error,
    path_obj: Option<&Object>,
    display: &str,
    display2: &str,
) -> RuntimeError {
    match path_obj {
        Some(o) => crate::error::io_error_to_py_path2(e, o, display, display2),
        None => crate::error::io_error_to_py_named2(e, Some(display), Some(display2)),
    }
}

fn os_remove(args: &[Object]) -> Result<Object, RuntimeError> {
    let p = first_path(args, "remove")?;
    std::fs::remove_file(&p).map_err(|e| path_io_err(&e, args.first(), &p))?;
    Ok(Object::None)
}

/// `os.unlink(path, *, dir_fd=None)` / `os.remove`. With `dir_fd` set the
/// removal is `unlinkat`-relative (RFC 0040 WS1; `shutil.rmtree`'s safe path
/// unlinks each entry relative to its parent directory's descriptor).
fn os_remove_kw(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    #[cfg(unix)]
    if let Some(dfd) = dir_fd_arg(kwargs)? {
        let p = first_path(args, "unlink")?;
        let cpath =
            std::ffi::CString::new(p.as_bytes()).map_err(|_| value_error("embedded null byte"))?;
        let rc = unsafe { libc::unlinkat(dfd, cpath.as_ptr(), 0) };
        if rc != 0 {
            return Err(path_io_err(
                &std::io::Error::last_os_error(),
                args.first(),
                &p,
            ));
        }
        return Ok(Object::None);
    }
    #[cfg(not(unix))]
    reject_dir_fd(kwargs, "unlink")?;
    os_remove(args)
}

/// `os.rmdir(path, *, dir_fd=None)`. With `dir_fd` set the removal is
/// `unlinkat(..., AT_REMOVEDIR)`-relative (RFC 0040 WS1).
fn os_rmdir_kw(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    #[cfg(unix)]
    if let Some(dfd) = dir_fd_arg(kwargs)? {
        let p = first_path(args, "rmdir")?;
        let cpath =
            std::ffi::CString::new(p.as_bytes()).map_err(|_| value_error("embedded null byte"))?;
        let rc = unsafe { libc::unlinkat(dfd, cpath.as_ptr(), libc::AT_REMOVEDIR) };
        if rc != 0 {
            return Err(path_io_err(
                &std::io::Error::last_os_error(),
                args.first(),
                &p,
            ));
        }
        return Ok(Object::None);
    }
    #[cfg(not(unix))]
    reject_dir_fd(kwargs, "rmdir")?;
    os_rmdir(args)
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

/// `os.mkdir(path, mode=0o777, *, dir_fd=None)`. With `dir_fd` set the
/// directory is created `mkdirat`-relative (RFC 0040 WS1) — the descent
/// primitive for building/walking trees deeper than `PATH_MAX`.
fn os_mkdir_kw(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    #[cfg(unix)]
    if let Some(dfd) = dir_fd_arg(kwargs)? {
        let p = first_path(args, "mkdir")?;
        let mode = match args.get(1) {
            Some(m) => mode_arg(m, "mkdir")?,
            None => 0o777,
        };
        let cpath =
            std::ffi::CString::new(p.as_bytes()).map_err(|_| value_error("embedded null byte"))?;
        let rc = unsafe { libc::mkdirat(dfd, cpath.as_ptr(), mode as libc::mode_t) };
        if rc != 0 {
            return Err(path_io_err(
                &std::io::Error::last_os_error(),
                args.first(),
                &p,
            ));
        }
        return Ok(Object::None);
    }
    #[cfg(not(unix))]
    reject_dir_fd(kwargs, "mkdir")?;
    os_mkdir(args)
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
                exist_ok =
                    matches!(v, Object::Bool(true)) || matches!(v, Object::Int(n) if *n != 0);
            }
            // `mode` is consumed below (applied to the leaf `mkdir`); accept
            // it here so it isn't rejected as an unexpected keyword.
            "mode" => {}
            other => {
                return Err(crate::error::type_error(format!(
                    "makedirs() got an unexpected keyword argument '{other}'"
                )));
            }
        }
    }
    // `mode` is honoured for the *leaf* directory only, exactly like CPython:
    // the recursive call that materialises intermediate parents uses the
    // default `0o777` (`test_os.MakedirTests.test_mode` asserts the parent is
    // `0o775` under umask `0o002` while the leaf is `0o555`).
    let mode = args
        .get(1)
        .and_then(Object::as_i64)
        .or_else(|| {
            kwargs
                .iter()
                .find(|(k, _)| k == "mode")
                .and_then(|(_, v)| v.as_i64())
        })
        .map(|m| m as u32)
        .unwrap_or(0o777);
    // Faithful port of CPython's `os.makedirs` recursion (Lib/os.py): split
    // off the leaf, recurse to create the parent chain (skipping the work
    // when the head already exists), and special-case a trailing `os.curdir`
    // component (`xxx/newdir/.` is satisfied once `xxx/newdir` exists). Rust's
    // `create_dir_all` collapses a trailing `/.` incorrectly and ignores the
    // mode, so we don't use it (`test_os.MakedirTests.test_makedir`).
    makedirs_recursive(&p, mode, exist_ok)
        .map_err(|(e, path)| crate::error::io_error_to_py_named(&e, Some(&path)))?;
    Ok(Object::None)
}

/// `os.path.split` for a POSIX path string: returns `(head, tail)` where
/// `tail` is the last component and `head` keeps its trailing separators
/// stripped (unless it is all separators). Mirrors `posixpath.split`.
#[cfg(not(windows))]
fn posix_split(p: &str) -> (&str, &str) {
    match p.rfind('/') {
        Some(i) => {
            let (head_with_sep, tail) = p.split_at(i + 1);
            let trimmed = head_with_sep.trim_end_matches('/');
            let head = if trimmed.is_empty() {
                head_with_sep
            } else {
                trimmed
            };
            (head, tail)
        }
        None => ("", p),
    }
}

/// `ntpath.splitdrive`: peel the drive letter (`C:`), UNC share
/// (`\\server\share`), or device/verbatim prefix (`\\?\C:`, `\\.\pipe`)
/// off the front so the split below never treats it as a component.
#[cfg(windows)]
fn nt_splitdrive(p: &str) -> (&str, &str) {
    let is_sep = |c: char| c == '\\' || c == '/';
    let b = p.as_bytes();
    if b.len() >= 2 {
        if is_sep(b[0] as char) && is_sep(b[1] as char) {
            // `\\server\share` / `\\?\C:` — the drive runs through the
            // second component (ntpath.splitroot's UNC arm); a path
            // with no second component is all drive.
            let rest = &p[2..];
            if let Some(i) = rest.find(is_sep) {
                if let Some(j) = rest[i + 1..].find(is_sep) {
                    let cut = 2 + i + 1 + j;
                    return (&p[..cut], &p[cut..]);
                }
            }
            return (p, "");
        }
        if b[1] == b':' && b[0].is_ascii_alphabetic() {
            return (&p[..2], &p[2..]);
        }
    }
    ("", p)
}

/// `ntpath.split`: like [`posix_split`] but with both separators and the
/// drive/UNC prefix kept attached to `head` (never split into, never
/// stripped down to nothing — `C:\` stays `C:\`).
#[cfg(windows)]
fn nt_split(p: &str) -> (&str, &str) {
    let is_sep = |c: char| c == '\\' || c == '/';
    let (drive, rest) = nt_splitdrive(p);
    let i = rest.rfind(is_sep).map_or(0, |i| i + 1);
    let (head, tail) = rest.split_at(i);
    let trimmed = head.trim_end_matches(is_sep);
    let head_len = if trimmed.is_empty() {
        head.len()
    } else {
        trimmed.len()
    };
    (&p[..drive.len() + head_len], tail)
}

/// `os.path.split` for the host platform, as `os.makedirs`' recursion
/// requires: CPython's `makedirs` splits with `os.path.split`, so on
/// Windows the backslash-separated paths every `os.path.normpath`
/// consumer produces (sysconfig hands venv `{base}\Lib\site-packages`)
/// must split on `\` too, or the parent chain is never created and the
/// leaf `mkdir` dies with ERROR_PATH_NOT_FOUND.
fn host_path_split(p: &str) -> (&str, &str) {
    #[cfg(windows)]
    {
        nt_split(p)
    }
    #[cfg(not(windows))]
    {
        posix_split(p)
    }
}

/// Create a single directory with `mode` (umask still applies via `mkdir(2)`).
#[cfg(unix)]
fn mkdir_one(path: &str, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new().mode(mode).create(path)
}

#[cfg(not(unix))]
fn mkdir_one(path: &str, _mode: u32) -> std::io::Result<()> {
    std::fs::create_dir(path)
}

/// Faithful port of `os.makedirs(name, mode, exist_ok)`. Returns the failing
/// path alongside the error so the caller can set `OSError.filename`.
fn makedirs_recursive(
    name: &str,
    mode: u32,
    exist_ok: bool,
) -> Result<(), (std::io::Error, String)> {
    let (mut head, mut tail) = host_path_split(name);
    if tail.is_empty() {
        let (h, t) = host_path_split(head);
        head = h;
        tail = t;
    }
    if !head.is_empty() && !tail.is_empty() && !std::path::Path::new(head).exists() {
        match makedirs_recursive(head, 0o777, exist_ok) {
            Ok(()) => {}
            // A concurrently-created parent is fine (CPython's `except
            // FileExistsError: pass`).
            Err((e, _)) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
        // `xxx/newdir/.` — the curdir leaf already exists now that the parent
        // does, so don't try to `mkdir` it.
        if tail == "." {
            return Ok(());
        }
    }
    match mkdir_one(name, mode) {
        Ok(()) => Ok(()),
        Err(e) => {
            if !exist_ok || !std::path::Path::new(name).is_dir() {
                Err((e, name.to_owned()))
            } else {
                Ok(())
            }
        }
    }
}

fn os_rmdir(args: &[Object]) -> Result<Object, RuntimeError> {
    let p = first_path(args, "rmdir")?;
    std::fs::remove_dir(&p).map_err(|e| path_io_err(&e, args.first(), &p))?;
    Ok(Object::None)
}

fn os_rename(args: &[Object]) -> Result<Object, RuntimeError> {
    let src = first_path(args, "rename")?;
    let dst = nth_path(args, 1, "rename")?;
    std::fs::rename(&src, &dst).map_err(|e| path_io_err2(&e, args.first(), &src, &dst))?;
    Ok(Object::None)
}

fn os_listdir(args: &[Object]) -> Result<Object, RuntimeError> {
    // `os.listdir(fd)` — list a directory referred to by an open descriptor
    // (RFC 0040 WS1). `test_shutil`'s `_use_fd_functions` recomputation probes
    // `os.listdir in os.supports_fd`, so this and `os.scandir(fd)` must agree.
    #[cfg(unix)]
    match args.first() {
        Some(Object::Bool(b)) => {
            warn_bool_as_fd()?;
            return listdir_fd(libc::c_int::from(*b));
        }
        Some(Object::Int(n)) => return listdir_fd(*n as libc::c_int),
        _ => {}
    }
    // CPython: `listdir(path='.')`. `path` may be str, bytes, or any
    // `os.PathLike` (a `pathlib.Path`, which is what `Path.walk()` passes).
    // A `bytes` path yields `bytes` names; everything else yields `str`.
    let (p, want_bytes) = match args.first() {
        None | Some(Object::None) => (".".to_string(), false),
        Some(Object::Bytes(b)) => (String::from_utf8_lossy(b).into_owned(), true),
        // CPython's path converter accepts str/bytes/PathLike but rejects the
        // bytes-*like* `bytearray`/`memoryview` (`test_listdir_bytes_like`).
        Some(other @ (Object::ByteArray(_) | Object::MemoryView(_))) => {
            return Err(type_error(format!(
                "listdir: path should be string, bytes or os.PathLike, not {}",
                other.type_name()
            )));
        }
        Some(other) => (path_to_string(other, "listdir")?, false),
    };
    let mut out = Vec::new();
    let iter = std::fs::read_dir(&p).map_err(|e| path_io_err(&e, args.first(), &p))?;
    for entry in iter {
        let entry = entry.map_err(|e| path_io_err(&e, args.first(), &p))?;
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

/// Fill `buf` with cryptographically-strong OS randomness *without using a
/// file descriptor* — `getentropy` on macOS/BSD, the `getrandom` syscall on
/// Linux — falling back to `/dev/urandom` only where neither exists. The
/// fd-free path is what lets `os.urandom` keep working under a depleted
/// `RLIMIT_NOFILE` (and matches the `HAVE_GETENTROPY`/`HAVE_GETRANDOM`
/// `sysconfig` vars WeavePy advertises).
#[cfg(unix)]
fn fill_os_random(buf: &mut [u8]) -> std::io::Result<()> {
    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd"
    ))]
    {
        // `getentropy(2)` caps each request at 256 bytes (GETENTROPY_MAX).
        for chunk in buf.chunks_mut(256) {
            let rc = unsafe { libc::getentropy(chunk.as_mut_ptr().cast(), chunk.len()) };
            if rc != 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    {
        let mut filled = 0usize;
        while filled < buf.len() {
            let rc = unsafe {
                libc::getrandom(buf[filled..].as_mut_ptr().cast(), buf.len() - filled, 0)
            };
            if rc < 0 {
                let e = std::io::Error::last_os_error();
                // PEP 475: retry an interrupted syscall.
                if e.raw_os_error() == Some(libc::EINTR) {
                    service_pending_signals().map_err(|_| {
                        std::io::Error::new(std::io::ErrorKind::Interrupted, "interrupted")
                    })?;
                    continue;
                }
                return Err(e);
            }
            filled += rc as usize;
        }
        return Ok(());
    }
    #[allow(unreachable_code)]
    {
        // Other Unix: read the kernel CSPRNG device.
        use std::io::Read;
        let mut f = std::fs::File::open("/dev/urandom")?;
        f.read_exact(buf)?;
        Ok(())
    }
}

fn os_urandom(args: &[Object]) -> Result<Object, RuntimeError> {
    let n = match args.first().and_then(Object::as_i64) {
        // CPython rejects a negative size with `ValueError`.
        Some(n) if n < 0 => return Err(value_error("negative argument not allowed")),
        Some(n) => n as usize,
        // An int beyond ssize_t overflows the clinic conversion
        // (SystemRandom.randbytes(1 << 1000) — test_random expects
        // OverflowError, not TypeError).
        None if matches!(args.first(), Some(Object::Long(_))) => {
            return Err(crate::error::overflow_error(
                "Python int too large to convert to C ssize_t",
            ))
        }
        None => return Err(type_error("urandom() argument must be int")),
    };
    #[cfg(unix)]
    {
        let mut out = vec![0u8; n];
        fill_os_random(&mut out).map_err(|e| crate::error::io_error_to_py(&e))?;
        Ok(Object::new_bytes(out))
    }
    // Windows: the system-preferred CSPRNG, exactly CPython's
    // `_PyOS_URandom` → `BCryptGenRandom(NULL, …,
    // BCRYPT_USE_SYSTEM_PREFERRED_RNG)` (Python/bootstrap_hash.c).
    #[cfg(windows)]
    {
        use windows_sys::Win32::Security::Cryptography::{
            BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        };
        let mut out = vec![0u8; n];
        // BCryptGenRandom takes a u32 length; chunk absurdly large requests.
        for chunk in out.chunks_mut(1 << 30) {
            let status = unsafe {
                BCryptGenRandom(
                    std::ptr::null_mut(),
                    chunk.as_mut_ptr(),
                    chunk.len() as u32,
                    BCRYPT_USE_SYSTEM_PREFERRED_RNG,
                )
            };
            if status != 0 {
                return Err(crate::error::os_error(format!(
                    "BCryptGenRandom failed (NTSTATUS 0x{status:08X})"
                )));
            }
        }
        Ok(Object::new_bytes(out))
    }
    #[cfg(not(any(unix, windows)))]
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

/// Windows: close the CRT fd with `_close` (which closes the owned handle,
/// CPython's `os_close_impl`) and drop any registry entry naming it so a
/// `Disk`-backed stream that minted this fd doesn't double-close.
#[cfg(windows)]
fn os_close_fd(fd: i64) -> Result<Object, RuntimeError> {
    use crate::stdlib::nt_support::{self, crt};
    let fd = i32::try_from(fd).map_err(|_| value_error("file descriptor out of range"))?;
    let rc = unsafe { crt::_close(fd) };
    if rc != 0 {
        return Err(nt_support::last_crt_error_to_py(None));
    }
    nt_support::forget_fd(fd);
    Ok(Object::None)
}

#[cfg(not(any(unix, windows)))]
fn os_close_fd(_fd: i64) -> Result<Object, RuntimeError> {
    Ok(Object::None)
}

/// `os.open(path, flags, mode=0o777)` → raw fd. The flag bits are the
/// module's own `O_*` constants, which are the host libc's values, so
/// they pass straight to `open(2)`/`openat(2)`.
#[cfg(unix)]
fn os_open_stub(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    // CPython: `os.open(path, flags, mode=0o777, *, dir_fd=None)` — every
    // parameter is also accepted by keyword (`test_os.test_open_keywords`).
    let p = path_arg_or_kw(args, 0, "path", kwargs, "open")?;
    let flags = int_arg_or_kw(args, 1, "flags", kwargs)
        .ok_or_else(|| crate::error::type_error("open() flags must be an int".to_owned()))?;
    // `open(path, flags, mode=0o777)` — `mode` only matters when `O_CREAT`
    // creates the file; the kernel masks it with the umask, so
    // `Path.touch(0o444)` lands `0o444 & ~umask` (test_pathlib.test_touch_mode).
    let mode = int_arg_or_kw(args, 2, "mode", kwargs).unwrap_or(0o777) as u32;
    // RFC 0040 WS1 — `dir_fd=`-relative open via `openat`. `shutil.rmtree`'s
    // fd-based safe path (`_rmtree_safe_fd`) opens each subdirectory relative
    // to its parent's descriptor; the flag bits are already the host `O_*`
    // values, so they pass straight to `openat`.
    if let Some(dfd) = dir_fd_arg(kwargs)? {
        let cpath =
            std::ffi::CString::new(p.as_bytes()).map_err(|_| value_error("embedded null byte"))?;
        let fd = unsafe { libc::openat(dfd, cpath.as_ptr(), flags as libc::c_int, mode) };
        if fd < 0 {
            let e = std::io::Error::last_os_error();
            let path_obj = args.first();
            return Err(path_io_err(&e, path_obj, &p));
        }
        return Ok(Object::Int(i64::from(fd)));
    }
    // Hand the flag bits straight to the kernel. Routing through
    // `std::fs::OpenOptions` imposed Rust's own validation on top of
    // POSIX — notably rejecting `O_CREAT` with a read-only access mode
    // ("creating or truncating a file requires write or append
    // access"), which POSIX permits and `test_zipimport.
    // testFileUnreadable` exercises (`os.open(p, os.O_CREAT, 000)`).
    // Preserve the identity of the original `path` argument (positional or the
    // `path=` keyword) as `.filename` (`test_os.test_oserror_filename`).
    let path_obj = args.first().cloned().or_else(|| {
        kwargs
            .iter()
            .find(|(k, _)| k == "path")
            .map(|(_, v)| v.clone())
    });
    let cpath =
        std::ffi::CString::new(p.as_bytes()).map_err(|_| value_error("embedded null byte"))?;
    // PEP 446: descriptors Python creates are non-inheritable —
    // CPython's `os.open` ORs in `O_CLOEXEC` (as did the previous
    // `OpenOptions`-based implementation here).
    let fd = unsafe {
        libc::open(
            cpath.as_ptr(),
            flags as libc::c_int | libc::O_CLOEXEC,
            mode as libc::c_uint,
        )
    };
    if fd < 0 {
        let e = std::io::Error::last_os_error();
        return Err(path_io_err(&e, path_obj.as_ref(), &p));
    }
    Ok(Object::Int(i64::from(fd)))
}

/// Windows `os.open` — CPython's `os_open_impl` on the CRT-fd model: the
/// flag bits are the CRT's own values (published as `os.O_*` above) and the
/// open goes through `_wsopen_s` with `_SH_DENYNO` sharing.
#[cfg(windows)]
fn os_open_stub(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    use crate::stdlib::nt_support::{crt, crt_error_to_py, wide};
    let p = path_arg_or_kw(args, 0, "path", kwargs, "open")?;
    let flags = int_arg_or_kw(args, 1, "flags", kwargs)
        .ok_or_else(|| crate::error::type_error("open() flags must be an int".to_owned()))?;
    // `mode` feeds the CRT pmode (only `_S_IREAD`/`_S_IWRITE` matter); CPython
    // passes it through untranslated.
    let mode = int_arg_or_kw(args, 2, "mode", kwargs).unwrap_or(0o777) as i32;
    // No `openat` on NT — CPython rejects a non-None `dir_fd` the same way.
    reject_dir_fd(kwargs, "open")?;
    if p.as_bytes().contains(&0) {
        return Err(value_error("embedded null byte"));
    }
    // PEP 446: descriptors Python creates are non-inheritable — CPython's
    // `os_open_impl` ORs in `O_NOINHERIT` (the CRT spelling of `O_CLOEXEC`).
    let mut oflags = flags as i32 | crt::O_NOINHERIT;
    // CPython initialises the CRT with the *binary* default fmode
    // (`_Py_InitializeCore` / config->legacy_windows_fs_encoding path sets
    // `_set_fmode(_O_BINARY)`), so an `os.open` with no explicit text bit
    // yields a binary fd. WeavePy doesn't flip the process-global CRT
    // default; passing `O_BINARY` explicitly when no text/binary bit is set
    // is behaviourally identical and keeps the CRT state untouched.
    if oflags & (crt::O_TEXT | crt::O_WTEXT | crt::O_U16TEXT | crt::O_U8TEXT | crt::O_BINARY) == 0 {
        oflags |= crt::O_BINARY;
    }
    let wpath = wide(&p);
    let mut fd: i32 = -1;
    // `_wsopen_s` returns the errno directly (not through the TLS `errno`).
    let err = unsafe { crt::_wsopen_s(&raw mut fd, wpath.as_ptr(), oflags, crt::SH_DENYNO, mode) };
    if err != 0 {
        return Err(crt_error_to_py(err, Some(&p)));
    }
    Ok(Object::Int(i64::from(fd)))
}

#[cfg(not(any(unix, windows)))]
fn os_open_stub(_args: &[Object], _kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    Err(crate::error::not_implemented_error(
        "os.open(): raw fd interface is not implemented in WeavePy yet",
    ))
}

/// The `os.open` flag bits for a text/binary mode string, using the same
/// host-platform `O_*` values the `os` module exposes. `io.FileIO`/`io.open`
/// use this only to synthesize the `flags` argument handed to a user `opener`
/// callback, so it must agree with `os.O_*` (and hence the kernel) — otherwise
/// an opener that forwards to `os.open` would see Linux-flavoured bits on macOS.
pub(crate) fn open_flags_for_mode(mode: &str) -> i64 {
    let (o_wronly, o_rdwr, o_creat, o_excl, o_trunc, o_append) = open_flag_bits();
    let mut flags = if mode.contains('+') {
        o_rdwr
    } else if mode.contains('w') || mode.contains('a') || mode.contains('x') {
        o_wronly
    } else {
        0
    };
    if mode.contains('a') {
        flags |= o_append | o_creat;
    }
    if mode.contains('w') {
        flags |= o_creat | o_trunc;
    }
    if mode.contains('x') {
        flags |= o_creat | o_excl;
    }
    flags
}

#[cfg(unix)]
fn open_flag_bits() -> (i64, i64, i64, i64, i64, i64) {
    (
        i64::from(libc::O_WRONLY),
        i64::from(libc::O_RDWR),
        i64::from(libc::O_CREAT),
        i64::from(libc::O_EXCL),
        i64::from(libc::O_TRUNC),
        i64::from(libc::O_APPEND),
    )
}

// Windows: the CRT flag values, agreeing with the `os.O_*` constants the
// module publishes (an `opener` that forwards to `os.open` must see the
// same bits `_wsopen_s` understands).
#[cfg(windows)]
fn open_flag_bits() -> (i64, i64, i64, i64, i64, i64) {
    use crate::stdlib::nt_support::crt;
    (
        i64::from(crt::O_WRONLY),
        i64::from(crt::O_RDWR),
        i64::from(crt::O_CREAT),
        i64::from(crt::O_EXCL),
        i64::from(crt::O_TRUNC),
        i64::from(crt::O_APPEND),
    )
}

#[cfg(not(any(unix, windows)))]
fn open_flag_bits() -> (i64, i64, i64, i64, i64, i64) {
    (1, 2, 64, 128, 512, 1024)
}

/// `posix._fcopyfile(in_fd, out_fd, flags)` — macOS-only wrapper over
/// `fcopyfile(3)`, mirroring CPython's `os__fcopyfile_impl`. `shutil`'s
/// `_fastcopy_fcopyfile` calls this with two file descriptors and a
/// `_COPYFILE_*` flag mask for a copy-on-write clone on APFS/HFS+.
#[cfg(target_os = "macos")]
fn os_fcopyfile(args: &[Object]) -> Result<Object, RuntimeError> {
    extern "C" {
        fn fcopyfile(
            from: libc::c_int,
            to: libc::c_int,
            state: *mut libc::c_void,
            flags: u32,
        ) -> libc::c_int;
    }
    let in_fd = args
        .first()
        .and_then(crate::object::Object::as_i64)
        .ok_or_else(|| crate::error::type_error("_fcopyfile() in_fd must be an int".to_owned()))?
        as libc::c_int;
    let out_fd = args
        .get(1)
        .and_then(crate::object::Object::as_i64)
        .ok_or_else(|| crate::error::type_error("_fcopyfile() out_fd must be an int".to_owned()))?
        as libc::c_int;
    let flags = args
        .get(2)
        .and_then(crate::object::Object::as_i64)
        .ok_or_else(|| crate::error::type_error("_fcopyfile() flags must be an int".to_owned()))?
        as u32;
    // SAFETY: `in_fd`/`out_fd` are caller-supplied descriptors; a NULL
    // `copyfile_state_t` is the documented "no state" form.
    let rc = unsafe { fcopyfile(in_fd, out_fd, std::ptr::null_mut(), flags) };
    if rc < 0 {
        return Err(crate::error::io_error_to_py(
            &std::io::Error::last_os_error(),
        ));
    }
    Ok(Object::None)
}

/// `os.fdopen(fd, mode='r', ...)` — wrap an existing OS file descriptor in a
/// file object (CPython returns `io.open(fd, ...)`). WeavePy adopts the fd
/// into a `Disk`-backed `PyFile`, so `read`/`write`/`seek`/`fileno` work and
/// closing the file closes the fd.
#[cfg(unix)]
fn os_fdopen(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    use crate::object::{FileBackend, PyFile};
    use std::os::unix::io::FromRawFd;
    // CPython 3.12+: a `bool` fd raises a `RuntimeWarning` before anything
    // else (`test_os.TestInvalidFD.test_fdopen` runs the bool check under
    // `simplefilter("error", RuntimeWarning)`).
    if matches!(args.first(), Some(Object::Bool(_))) {
        warn_bool_as_fd()?;
    }
    let fd = args
        .first()
        .and_then(crate::object::Object::as_i64)
        .ok_or_else(|| crate::error::type_error("fdopen() fd must be an int".to_owned()))?;
    // CPython implements `os.fdopen` as `io.open(fd, ...)`, which `fstat`s the
    // descriptor and raises `OSError(EBADF)` for an invalid fd
    // (`test_os.test_fdopen`'s `check`). Validate before wrapping so a bad fd
    // surfaces immediately rather than on first I/O.
    {
        let rc = unsafe { libc::fcntl(fd as i32, libc::F_GETFD) };
        if rc < 0 {
            return Err(crate::error::io_error_to_py(
                &std::io::Error::last_os_error(),
            ));
        }
    }
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
    // CPython's `os.fdopen` *is* `io.open(fd, …)`: the text-layer
    // configuration (buffering / encoding / errors / newline) applies the
    // same way — fileinput's inplace mode fdopens its output with
    // `encoding=`/`errors=` and expects the codec to run on writes
    // (test_fileinput.test_inplace_encoding_errors).
    let kw = |name: &str| kwargs.iter().find(|(k, _)| k == name).map(|(_, v)| v);
    let buffering = args.get(2).or_else(|| kw("buffering"));
    let encoding = args.get(3).or_else(|| kw("encoding"));
    let errors = args.get(4).or_else(|| kw("errors"));
    let newline = args.get(5).or_else(|| kw("newline"));
    let binary = pf.binary;
    crate::stdlib::io_full::finish_open(
        Object::File(Rc::new(pf)),
        buffering,
        encoding,
        errors,
        newline,
        binary,
    )
}

/// Windows `os.fdopen` — adopt a CRT fd into a `Disk`-backed `PyFile`. The
/// registry entry recorded by `owning_file_from_fd` routes the eventual
/// close back through `_close(fd)` (the fd owns the handle, RFC 0063).
#[cfg(windows)]
fn os_fdopen(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    use crate::object::{FileBackend, PyFile};
    // CPython 3.12+: a `bool` fd raises a `RuntimeWarning` before anything
    // else (mirrors the Unix arm).
    if matches!(args.first(), Some(Object::Bool(_))) {
        warn_bool_as_fd()?;
    }
    let fd = args
        .first()
        .and_then(crate::object::Object::as_i64)
        .ok_or_else(|| crate::error::type_error("fdopen() fd must be an int".to_owned()))?;
    let fd = i32::try_from(fd).map_err(|_| value_error("file descriptor out of range"))?;
    // CPython's `io.open(fd, …)` fstats the descriptor and raises
    // `OSError(EBADF)` for an invalid fd; `_get_osfhandle` inside
    // `owning_file_from_fd` performs the equivalent validation.
    let file = crate::stdlib::nt_support::owning_file_from_fd(fd)
        .map_err(|_| crate::stdlib::nt_support::crt_error_to_py(crate::py_errno::EBADF, None))?;
    let mode = match args.get(1) {
        Some(Object::Str(s)) => s.to_string(),
        None => "r".to_owned(),
        Some(_) => {
            return Err(crate::error::type_error(
                "fdopen() mode must be str".to_owned(),
            ))
        }
    };
    let pf = PyFile::new(format!("<fdopen fd={fd}>"), mode, FileBackend::Disk(file));
    pf.no_name.set(true);
    let kw = |name: &str| kwargs.iter().find(|(k, _)| k == name).map(|(_, v)| v);
    let buffering = args.get(2).or_else(|| kw("buffering"));
    let encoding = args.get(3).or_else(|| kw("encoding"));
    let errors = args.get(4).or_else(|| kw("errors"));
    let newline = args.get(5).or_else(|| kw("newline"));
    let binary = pf.binary;
    crate::stdlib::io_full::finish_open(
        Object::File(Rc::new(pf)),
        buffering,
        encoding,
        errors,
        newline,
        binary,
    )
}

#[cfg(not(any(unix, windows)))]
fn os_fdopen(_args: &[Object], _kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    Err(crate::error::not_implemented_error(
        "os.fdopen(): raw fd interface is not implemented in WeavePy yet",
    ))
}

/// `os.stat(path, *, dir_fd=None, follow_symlinks=True)`. `follow_symlinks=False`
/// makes it an `lstat` (the link itself); `shutil.copystat`/`copy2` and
/// `pathlib`/`tempfile` pass the keyword. `dir_fd` is unsupported (only `None`).
fn os_stat_kw(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    // RFC 0040 WS1 — `os.stat(path, dir_fd=fd, follow_symlinks=…)` via
    // `fstatat`. `shutil.rmtree`'s safe path and `os.supports_dir_fd`
    // membership depend on this.
    #[cfg(unix)]
    if let Some(dfd) = dir_fd_arg(kwargs)? {
        let p = first_path(args, "stat")?;
        return fstatat_stat_result(dfd, &p, dir_entry_follow(kwargs), args.first());
    }
    #[cfg(not(unix))]
    reject_dir_fd(kwargs, "stat")?;
    // `os.stat(fd)` (an int) is `fstat`; `os.stat(path)` hits the filesystem.
    // `genericpath.exists`/`isfile`/… lean on the fd form when handed a
    // descriptor.
    if let Some(Object::Int(_) | Object::Bool(_)) = args.first() {
        // `follow_symlinks` is meaningless for a descriptor; CPython rejects
        // the combination (`test_posix.test_stat_fd_zero_follow_symlinks`).
        let follow_explicit = kwargs.iter().any(|(k, _)| k == "follow_symlinks");
        if follow_explicit && !dir_entry_follow(kwargs) {
            return Err(value_error("cannot use fd and follow_symlinks together"));
        }
        return os_fstat(args);
    }
    // The `stat`/`fstat` path-or-fd converter accepts str/bytes/PathLike or an
    // integer fd — but *not* `bytearray`, `None`, `float`, … . Reject those
    // eagerly with CPython's "or integer" wording
    // (`test_posix.test_stat`/`test_fstat`).
    match args.first() {
        Some(Object::Str(_) | Object::WStr(_) | Object::Bytes(_) | Object::Instance(_)) => {}
        other => {
            let tn = other.map_or("NoneType".to_string(), |o| o.type_name().to_string());
            return Err(type_error(format!(
                "stat: path should be string, bytes, os.PathLike or integer, not {tn}"
            )));
        }
    }
    let p = first_path(args, "stat")?;
    let meta = if dir_entry_follow(kwargs) {
        std::fs::metadata(&p)
    } else {
        std::fs::symlink_metadata(&p)
    }
    .map_err(|e| path_io_err(&e, args.first(), &p))?;
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
pub(crate) fn warn_bool_as_fd() -> Result<(), RuntimeError> {
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
        // CPython's int converter overflows before it type-errors:
        // `os.fstat(2**1000)` must raise OverflowError, not TypeError
        // (`test_socket.test__sendfile_use_sendfile` asserts the pair).
        Some(Object::Long(_)) => {
            return Err(crate::error::overflow_error(
                "Python int too large to convert to C int",
            ))
        }
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
    // Windows: classify the fd's handle first (CPython's `_Py_fstat` does
    // `GetFileType` and synthesises `S_IFIFO`/`S_IFCHR` for pipes/console
    // handles, where `GetFileInformationByHandle` would fail), then read the
    // real metadata through a non-owning `File` view for disk files.
    #[cfg(windows)]
    {
        use crate::stdlib::nt_support;
        use windows_sys::Win32::Storage::FileSystem::{FILE_TYPE_CHAR, FILE_TYPE_PIPE};
        let fd = i32::try_from(fd).map_err(|_| value_error("file descriptor out of range"))?;
        let Some(ftype) = nt_support::file_type_of_fd(fd) else {
            return Err(nt_support::crt_error_to_py(crate::py_errno::EBADF, None));
        };
        match ftype {
            FILE_TYPE_PIPE => Ok(stat_result_synthetic(0o010_666)), // S_IFIFO
            FILE_TYPE_CHAR => Ok(stat_result_synthetic(0o020_666)), // S_IFCHR
            _ => {
                let view = nt_support::file_view_from_fd(fd)
                    .map_err(|_| nt_support::crt_error_to_py(crate::py_errno::EBADF, None))?;
                let meta = view
                    .metadata()
                    .map_err(|e| crate::error::io_error_to_py(&e))?;
                Ok(stat_result_from_meta(&meta))
            }
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = fd;
        Err(crate::error::not_implemented_error(
            "os.fstat is only supported on Unix",
        ))
    }
}

/// A `stat_result` for handles that have no filesystem identity (pipes,
/// console fds): only `st_mode` is meaningful, everything else is zero —
/// the shape CPython's `_Py_attribute_data_to_stat` produces for them.
#[cfg(windows)]
fn stat_result_synthetic(mode: i64) -> Object {
    use crate::types::PyInstance;
    let ty = stat_result_type();
    let inst = PyInstance::new(ty);
    {
        let mut d = inst.dict.borrow_mut();
        d.insert(DictKey(Object::from_static("st_mode")), Object::Int(mode));
        for f in [
            "st_ino", "st_dev", "st_nlink", "st_uid", "st_gid", "st_size",
        ] {
            d.insert(DictKey(Object::from_static(f)), Object::Int(0));
        }
        for f in ["st_atime", "st_mtime", "st_ctime"] {
            d.insert(DictKey(Object::from_static(f)), Object::Float(0.0));
        }
        for f in ["st_atime_ns", "st_mtime_ns", "st_ctime_ns"] {
            d.insert(DictKey(Object::from_static(f)), Object::Int(0));
        }
    }
    stat_seq_finish(&inst);
    Object::Instance(Rc::new(inst))
}

/// `os.lstat(path, *, dir_fd=None)` — `stat` on the link itself. `dir_fd` is
/// unsupported (only `None`).
fn os_lstat_kw(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    #[cfg(unix)]
    if let Some(dfd) = dir_fd_arg(kwargs)? {
        let p = first_path(args, "lstat")?;
        return fstatat_stat_result(dfd, &p, false, args.first());
    }
    #[cfg(not(unix))]
    reject_dir_fd(kwargs, "lstat")?;
    let p = first_path(args, "lstat")?;
    let meta = std::fs::symlink_metadata(&p).map_err(|e| path_io_err(&e, args.first(), &p))?;
    Ok(stat_result_from_meta(&meta))
}

/// Finish a Rust-built `stat_result`: give the instance its native 10-slot
/// tuple view (CPython's sequence layout, integer seconds in the three
/// unnamed time slots) and default any layout-advertised named field the
/// builder didn't set, so attribute access matches CPython on every platform.
fn stat_seq_finish(inst: &crate::types::PyInstance) {
    let mut seq: Vec<Object> = Vec::with_capacity(10);
    {
        let d = inst.dict.borrow();
        let get = |f: &'static str| d.get(&DictKey(Object::from_static(f))).cloned();
        for f in [
            "st_mode", "st_ino", "st_dev", "st_nlink", "st_uid", "st_gid", "st_size",
        ] {
            seq.push(get(f).unwrap_or(Object::Int(0)));
        }
        for f in ["st_atime", "st_mtime", "st_ctime"] {
            seq.push(match get(f) {
                Some(Object::Float(x)) => Object::Int(x as i64),
                Some(other) => other,
                None => Object::Int(0),
            });
        }
    }
    let _ = inst.native.set(Object::new_tuple(seq));
    #[cfg(target_os = "macos")]
    {
        let mut d = inst.dict.borrow_mut();
        for f in ["st_flags", "st_gen"] {
            let k = DictKey(Object::from_static(f));
            if d.get(&k).is_none() {
                d.insert(k, Object::Int(0));
            }
        }
        let k = DictKey(Object::from_static("st_birthtime"));
        if d.get(&k).is_none() {
            let v = d
                .get(&DictKey(Object::from_static("st_ctime")))
                .cloned()
                .unwrap_or(Object::Float(0.0));
            d.insert(k, v);
        }
    }
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
    // Windows-only extras CPython adds to `stat_result` (posixmodule.c's
    // `STRUCT_STAT` under `MS_WINDOWS`): the raw `dwFileAttributes` word —
    // `ntpath.isjunction`/`stat.FILE_ATTRIBUTE_*` consumers read it — and the
    // reparse tag (0 here: `std::fs::Metadata` doesn't surface the tag, and 0
    // is what CPython reports for non-reparse-point files).
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        d.insert(
            DictKey(Object::from_static("st_file_attributes")),
            Object::Int(i64::from(meta.file_attributes())),
        );
        d.insert(
            DictKey(Object::from_static("st_reparse_tag")),
            Object::Int(0),
        );
    }
    drop(d);
    stat_seq_finish(&inst);
    Object::Instance(Rc::new(inst))
}

/// Build a `stat_result` from a raw `libc::stat`, for the `*at` syscalls
/// (`fstatat`) that back `dir_fd=`-relative `os.stat`/`os.lstat` and the
/// `os.scandir(fd)` `DirEntry` methods. Mirrors [`stat_result_from_meta`]'s
/// Unix branch field-for-field so a `dir_fd` stat is indistinguishable from a
/// path stat.
#[cfg(unix)]
// `libc::stat` field widths are platform-dependent (e.g. `st_dev`/`st_rdev`/
// `st_nlink` are 32-/16-bit on macOS but 64-bit on Linux), so the `as i64`
// coercions are lossless on some targets and narrowing on others; a blanket
// `i64::from` won't compile on the 64-bit targets (no `From<u64> for i64`).
#[allow(clippy::cast_lossless)]
fn stat_result_from_libc_stat(st: &libc::stat) -> Object {
    use crate::types::PyInstance;
    let ty = stat_result_type();
    let inst = PyInstance::new(ty);
    {
        let mut d = inst.dict.borrow_mut();
        let ns = |s: i64, n: i64| (s as f64) + (n as f64) * 1e-9;
        let atime = ns(st.st_atime as i64, st.st_atime_nsec as i64);
        let mtime = ns(st.st_mtime as i64, st.st_mtime_nsec as i64);
        let ctime = ns(st.st_ctime as i64, st.st_ctime_nsec as i64);
        for (k, v) in [
            ("st_mode", i64::from(st.st_mode)),
            ("st_ino", st.st_ino as i64),
            ("st_dev", st.st_dev as i64),
            ("st_nlink", st.st_nlink as i64),
            ("st_uid", i64::from(st.st_uid)),
            ("st_gid", i64::from(st.st_gid)),
            ("st_size", st.st_size as i64),
            ("st_rdev", st.st_rdev as i64),
            ("st_blocks", st.st_blocks as i64),
            ("st_blksize", st.st_blksize as i64),
            (
                "st_mtime_ns",
                st.st_mtime as i64 * 1_000_000_000 + st.st_mtime_nsec as i64,
            ),
            (
                "st_atime_ns",
                st.st_atime as i64 * 1_000_000_000 + st.st_atime_nsec as i64,
            ),
            (
                "st_ctime_ns",
                st.st_ctime as i64 * 1_000_000_000 + st.st_ctime_nsec as i64,
            ),
        ] {
            d.insert(DictKey(Object::from_static(k)), Object::Int(v));
        }
        for (k, v) in [
            ("st_mtime", mtime),
            ("st_atime", atime),
            ("st_ctime", ctime),
        ] {
            d.insert(DictKey(Object::from_static(k)), Object::Float(v));
        }
        // The BSD extras CPython exposes on macOS (`st_flags`, `st_gen`,
        // `st_birthtime`) come straight off the raw struct.
        #[cfg(target_os = "macos")]
        {
            d.insert(
                DictKey(Object::from_static("st_flags")),
                Object::Int(i64::from(st.st_flags)),
            );
            d.insert(
                DictKey(Object::from_static("st_gen")),
                Object::Int(i64::from(st.st_gen)),
            );
            d.insert(
                DictKey(Object::from_static("st_birthtime")),
                Object::Float(ns(st.st_birthtime as i64, st.st_birthtime_nsec as i64)),
            );
        }
    }
    stat_seq_finish(&inst);
    Object::Instance(Rc::new(inst))
}

/// Resolve an optional `dir_fd=` keyword. Returns `None` when absent or `None`
/// (the caller then takes its plain path-relative path), `Some(fd)` for an
/// integer descriptor. A non-int, non-`None` value is a `TypeError` like
/// CPython's `dir_fd` converter.
#[cfg(unix)]
fn dir_fd_arg(kwargs: &[(String, Object)]) -> Result<Option<libc::c_int>, RuntimeError> {
    match kwargs.iter().find(|(k, _)| k == "dir_fd").map(|(_, v)| v) {
        None | Some(Object::None) => Ok(None),
        // A `bool` descriptor warns ("bool is used as a file descriptor"),
        // then is used as 0/1 — CPython's `dir_fd` converter calls the same
        // `_PyLong_FileDescriptor_Converter` path (`test_posix.test_stat_dir_fd`).
        Some(Object::Bool(b)) => {
            warn_bool_as_fd()?;
            Ok(Some(libc::c_int::from(*b)))
        }
        // An in-range `int` fits; an out-of-range one (or any bignum `int`,
        // which is by definition past `i64` let alone `c_int`) raises
        // `OverflowError`, matching CPython's `PyLong_AsInt` — *not* `TypeError`
        // (`posix.stat(name, dir_fd=10**20)` → OverflowError).
        Some(Object::Int(n)) => libc::c_int::try_from(*n)
            .map(Some)
            .map_err(|_| crate::error::overflow_error("Python int too large to convert to C int")),
        Some(Object::Long(_)) => Err(crate::error::overflow_error(
            "Python int too large to convert to C int",
        )),
        Some(other) => Err(type_error(format!(
            "argument should be integer or None, not {}",
            other.type_name()
        ))),
    }
}

/// `fstatat(dir_fd, path, follow_symlinks)` → `stat_result`, the engine behind
/// `dir_fd=`-relative `os.stat`/`os.lstat` and the `os.scandir(fd)` entries.
#[cfg(unix)]
fn fstatat_stat_result(
    dir_fd: libc::c_int,
    path: &str,
    follow: bool,
    path_obj: Option<&Object>,
) -> Result<Object, RuntimeError> {
    let cpath =
        std::ffi::CString::new(path.as_bytes()).map_err(|_| value_error("embedded null byte"))?;
    let flags = if follow { 0 } else { libc::AT_SYMLINK_NOFOLLOW };
    // SAFETY: `st` is fully initialised by a successful `fstatat`; the path is
    // NUL-terminated and only read.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::fstatat(dir_fd, cpath.as_ptr(), &raw mut st, flags) };
    if rc != 0 {
        let e = std::io::Error::last_os_error();
        return Err(path_io_err(&e, path_obj, path));
    }
    Ok(stat_result_from_libc_stat(&st))
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
    let t = std::fs::read_link(&pstr).map_err(|e| path_io_err(&e, args.first(), &pstr))?;
    if want_bytes {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            return Ok(Object::new_bytes(t.into_os_string().into_vec()));
        }
        // No raw-bytes `OsString` view off Unix; fall back to the WTF-8
        // encoded form so a bytes-flavoured `readlink` still yields `bytes`.
        #[cfg(not(unix))]
        {
            return Ok(Object::new_bytes(t.into_os_string().into_encoded_bytes()));
        }
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
        // PEP 383: a lone-surrogate `str` path keeps its `str` flavour, but is
        // fsencoded (`surrogateescape`) for validation — a non-escapable
        // surrogate raises `UnicodeEncodeError` here, exactly like CPython's
        // `path_converter` (escapable U+DC80..U+DCFF survives lossily pending
        // the byte-faithful OsString syscall rewrite).
        Object::WStr(cps) => {
            let bytes =
                crate::stdlib::codecs_mod::encode_codepoints(cps, "utf-8", "surrogateescape")?;
            Ok(Object::from_str(
                String::from_utf8_lossy(&bytes).into_owned(),
            ))
        }
        Object::ByteArray(b) => Ok(Object::new_bytes(b.borrow().clone())),
        Object::Instance(_) => {
            if let Some(n @ (Object::Str(_) | Object::Bytes(_))) = obj.native_value() {
                return Ok(n);
            }
            let ptr = crate::vm_singletons::current_interpreter_ptr().ok_or_else(|| {
                type_error(format!(
                    "{func}: path should be string, bytes or os.PathLike, not {}",
                    obj.type_name_owned()
                ))
            })?;
            // SAFETY: published by the enclosing VM frame on this thread.
            let interp = unsafe { &mut *ptr };
            // `__fspath__` absent or explicitly `None` ⇒ not path-like.
            let fspath = match interp.load_attr_public(obj, "__fspath__") {
                Ok(Object::None) | Err(_) => {
                    return Err(type_error(format!(
                        "{func}: path should be string, bytes or os.PathLike, not {}",
                        obj.type_name_owned()
                    )))
                }
                Ok(m) => m,
            };
            match interp.call_object(fspath, &[], &[])? {
                r @ (Object::Str(_) | Object::Bytes(_)) => Ok(r),
                // Surrogate-bearing str result: apply the same PEP 383
                // fsencode-for-validation as a directly-passed WStr path.
                w @ Object::WStr(_) => resolve_fspath_obj(&w, func),
                Object::ByteArray(b) => Ok(Object::new_bytes(b.borrow().clone())),
                other => Err(type_error(format!(
                    "expected {}.__fspath__() to return str or bytes, not {}",
                    obj.type_name_owned(),
                    other.type_name_owned()
                ))),
            }
        }
        other => Err(type_error(format!(
            "{func}: path should be string, bytes or os.PathLike, not {}",
            other.type_name_owned()
        ))),
    }
}

fn os_chdir(args: &[Object]) -> Result<Object, RuntimeError> {
    let p = first_path(args, "chdir")?;
    // Attach the offending path so the raised OSError carries `.filename`
    // (CPython does this for path syscalls; subprocess's bad-cwd tests compare
    // `os.chdir(bad).filename` against the error surfaced from the child).
    std::env::set_current_dir(&p).map_err(|e| path_io_err(&e, args.first(), &p))?;
    Ok(Object::None)
}

pub(crate) fn os_fspath(args: &[Object]) -> Result<Object, RuntimeError> {
    let obj = match args.first() {
        Some(o) => o,
        None => return Err(type_error("fspath() takes exactly one argument")),
    };
    match obj {
        // A surrogate-bearing `WStr` is a `str` for path purposes (PEP 383).
        Object::Str(_) | Object::WStr(_) | Object::Bytes(_) => Ok(obj.clone()),
        Object::Instance(_) => {
            // A `str`/`bytes` subclass reduces to its native value (CPython
            // `os.fspath` returns those directly).
            if let Some(n @ (Object::Str(_) | Object::WStr(_) | Object::Bytes(_))) =
                obj.native_value()
            {
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
            // A type whose `__fspath__` is `None` is *explicitly* not
            // path-like (CPython treats `__fspath__ = None` like
            // `__hash__ = None`): raise the "expected str, bytes …" message
            // rather than letting the `None()` call surface a "NoneType is
            // not callable" (`test_os.TestPEP519.test_fspath_set_to_None`).
            let fspath = match interp.load_attr_public(obj, "__fspath__") {
                Ok(Object::None) | Err(_) => {
                    return Err(type_error(format!(
                        "expected str, bytes or os.PathLike object, not {}",
                        obj.type_name_owned()
                    )))
                }
                Ok(f) => f,
            };
            match interp.call_object(fspath, &[], &[])? {
                // `os.fspath` hands the PathLike's result back untouched;
                // a surrogate-bearing WStr *is* a str (PEP 383), matching
                // the direct-argument arm above.
                r @ (Object::Str(_) | Object::WStr(_) | Object::Bytes(_)) => Ok(r),
                other => Err(type_error(format!(
                    "expected {}.__fspath__() to return str or bytes, not {}",
                    obj.type_name_owned(),
                    other.type_name_owned()
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
        Object::Str(_) | Object::WStr(_) | Object::Bytes(_) => Ok(obj.clone()),
        Object::Instance(_) => match obj.native_value() {
            Some(n @ (Object::Str(_) | Object::WStr(_) | Object::Bytes(_))) => Ok(n),
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
        s @ (Object::Str(_) | Object::WStr(_)) => Ok(s),
        // PEP 383: decode with the filesystem encoding (UTF-8) and the
        // `surrogateescape` handler, so undecodable bytes become lone
        // surrogates that `fsencode` can map back to the original bytes.
        Object::Bytes(b) => {
            crate::stdlib::codecs_mod::decode_bytes_obj(&b, "utf-8", "surrogateescape")
        }
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
        // PEP 383: a surrogate-bearing path encodes with `surrogateescape`,
        // mapping U+DC80..U+DCFF back to the original raw bytes.
        w @ Object::WStr(_) => {
            let bytes = crate::stdlib::codecs_mod::encode_obj(&w, "utf-8", "surrogateescape")?;
            Ok(Object::Bytes(Rc::from(bytes.as_slice())))
        }
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
    // NB: unlike a generic path converter, CPython's `os.scandir` rejects
    // `bytearray`/`memoryview` (`test_os.test_bytes_like` expects `TypeError`):
    // only `str`, `bytes`, and `os.PathLike` flow through. `bytearray` is
    // therefore *not* matched here and lands in the catch-all `TypeError` arm.
    // `os.scandir(fd)` — iterate a directory referred to by an open file
    // descriptor (RFC 0040 WS1). `shutil.rmtree`'s safe path (`_rmtree_safe_fd`,
    // taken when `os.scandir in os.supports_fd`) opens each subdirectory and
    // scandirs it by fd to sidestep symlink races and `PATH_MAX`.
    #[cfg(unix)]
    match args.first() {
        Some(Object::Bool(b)) => {
            warn_bool_as_fd()?;
            return scandir_fd(libc::c_int::from(*b));
        }
        Some(Object::Int(n)) => return scandir_fd(*n as libc::c_int),
        _ => {}
    }
    let (dir_path, bytes_mode) = match args.first() {
        None | Some(Object::None) => (".".to_owned(), false),
        Some(Object::Str(s)) => (s.to_string(), false),
        Some(Object::Bytes(b)) => (String::from_utf8_lossy(b).into_owned(), true),
        // A lone-surrogate `str` path (PEP 383) routes through the shared
        // converter, which fsencodes it (`surrogateescape`) and raises
        // `UnicodeEncodeError` for a non-escapable surrogate.
        Some(other @ (Object::WStr(_) | Object::Instance(_))) => {
            (path_to_string(other, "scandir")?, false)
        }
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
            // CPython caches the inode from the directory read, so
            // `DirEntry.inode()` keeps working after the entry is unlinked
            // (`test_os.TestScandir.test_removed_{file,dir}`).
            let cached_inode = dir_entry_cached_inode(&entry);
            build_dir_entry(name_obj, path_obj, fs_path, cached_inode)
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

/// The inode number straight from the directory read (no `stat(2)` call), so
/// it survives the entry being removed. `None` off Unix (the
/// `inode()`/`stat()` accessors then fall back to a live `lstat`).
fn dir_entry_cached_inode(entry: &std::fs::DirEntry) -> Option<i64> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirEntryExt;
        Some(entry.ino() as i64)
    }
    #[cfg(not(unix))]
    {
        let _ = entry;
        None
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
        let mut dict = DictData::default();
        scandir_method(&mut dict, "__iter__", scandir_self);
        scandir_method(&mut dict, "__next__", scandir_next);
        scandir_method(&mut dict, "__enter__", scandir_self);
        scandir_method(&mut dict, "__exit__", scandir_exit);
        scandir_method(&mut dict, "close", scandir_exit);
        // CPython's `ScandirIterator` keeps an open `DIR*`; if it is dropped
        // without being exhausted or closed, `__del__` emits a
        // `ResourceWarning` (`test_os.TestScandir.test_resource_warning`).
        scandir_method(&mut dict, "__del__", scandir_del);
        // The iterator is only minted by `os.scandir`; calling its type
        // raises `TypeError` (`test_os.TestScandir.test_uninstantiable`).
        dict.insert(
            DictKey(Object::from_static("__new__")),
            builtin("__new__", |_args| {
                Err(type_error(
                    "cannot create 'posix.ScandirIterator' instances",
                ))
            }),
        );
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
        if let Some(Object::Iter(it)) = inst.native.get() {
            return match it.borrow_mut().next_value() {
                Some(v) => Ok(v),
                None => {
                    // Exhaustion closes the underlying directory handle in
                    // CPython, so a fully consumed iterator never warns.
                    scandir_mark_closed(inst);
                    Err(crate::error::stop_iteration())
                }
            };
        }
    }
    Err(type_error(
        "ScandirIterator.__next__ requires a scandir iterator",
    ))
}

fn scandir_exit(args: &[Object]) -> Result<Object, RuntimeError> {
    if let Some(Object::Instance(inst)) = args.first() {
        scandir_mark_closed(inst);
    }
    Ok(Object::None)
}

/// Sentinel key marking a `ScandirIterator` as closed/exhausted so its
/// `__del__` stays silent. Mirrors CPython closing the `DIR*` on `close()`,
/// `__exit__`, or exhaustion.
const SCANDIR_CLOSED_KEY: &str = "__weavepy_scandir_closed__";

fn scandir_mark_closed(inst: &crate::types::PyInstance) {
    inst.dict.borrow_mut().insert(
        DictKey(Object::from_static(SCANDIR_CLOSED_KEY)),
        Object::Bool(true),
    );
}

fn scandir_is_closed(inst: &crate::types::PyInstance) -> bool {
    matches!(
        inst.dict
            .borrow()
            .get(&DictKey(Object::from_static(SCANDIR_CLOSED_KEY))),
        Some(Object::Bool(true))
    )
}

/// `ScandirIterator.__del__`: warn if the iterator was never closed or
/// exhausted, matching CPython's `ResourceWarning` (`test_resource_warning`).
fn scandir_del(args: &[Object]) -> Result<Object, RuntimeError> {
    if let Some(Object::Instance(inst)) = args.first() {
        if !scandir_is_closed(inst) {
            // Latch closed first so a re-entrant finalisation can't double-warn.
            scandir_mark_closed(inst);
            if let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() {
                // SAFETY: published by the live VM driving this finaliser; the
                // GIL keeps the pointer exclusive on this thread.
                let interp = unsafe { &mut *ptr };
                return interp
                    .warn_resource_from_builtin("unclosed scandir iterator".to_owned())
                    .map(|()| Object::None);
            }
        }
    }
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
        let mut dict = DictData::default();
        // `os.DirEntry` is only ever minted by `scandir` (CPython's type has
        // no `tp_new`): `os.DirEntry()` raises `TypeError`
        // (`test_os.TestDirEntry.test_uninstantiable`). Internal construction
        // goes through `PyInstance::new`, which bypasses `__new__`.
        dict.insert(
            DictKey(Object::from_static("__new__")),
            builtin("__new__", |_args| {
                Err(type_error("cannot create 'posix.DirEntry' instances"))
            }),
        );
        // `repr(entry)` → `<DirEntry 'file.txt'>`. Dunders are resolved on the
        // *type*, so this lives here (not per-instance) and reads `self.name`
        // (`test_os.TestScandir.test_repr`).
        dict.insert(
            DictKey(Object::from_static("__repr__")),
            Object::Builtin(Rc::new(crate::object::BuiltinFn {
                name: "__repr__",
                binds_instance: true,
                call: Box::new(|args| {
                    let name = match args.first() {
                        Some(Object::Instance(i)) => i
                            .dict
                            .borrow()
                            .get(&DictKey(Object::from_static("name")))
                            .cloned()
                            .unwrap_or(Object::None),
                        _ => Object::None,
                    };
                    Ok(Object::from_str(format!("<DirEntry {}>", name.repr())))
                }),
                call_kw: None,
            })),
        );
        // CPython's `DirEntry` is unpicklable — `pickle.dumps(entry)` raises
        // `TypeError` (`test_os.TestDirEntry.test_unpickable`). Surface that
        // from `__reduce_ex__`/`__reduce__` (pickle calls `__reduce_ex__`).
        for hook in ["__reduce_ex__", "__reduce__"] {
            dict.insert(
                DictKey(Object::from_static(hook)),
                Object::Builtin(Rc::new(crate::object::BuiltinFn {
                    name: hook,
                    binds_instance: true,
                    call: Box::new(|_args| {
                        Err(type_error("cannot pickle 'posix.DirEntry' object"))
                    }),
                    call_kw: None,
                })),
            );
        }
        // `os.DirEntry[str]` → `types.GenericAlias` (CPython's C DirEntry
        // exposes `__class_getitem__ = Py_GenericAlias`;
        // test_genericalias generic_types sweep).
        dict.insert(
            DictKey(Object::from_static("__class_getitem__")),
            Object::ClassMethod(crate::object::MethodWrapper::new(Object::Builtin(Rc::new(
                crate::object::BuiltinFn {
                    name: "__class_getitem__",
                    binds_instance: true,
                    call: Box::new(|args| {
                        let origin = args.first().cloned().unwrap_or(Object::None);
                        let params = args.get(1).cloned().unwrap_or(Object::None);
                        Ok(crate::make_generic_alias_public(origin, params))
                    }),
                    call_kw: None,
                },
            )))),
        );
        let cls = TypeObject::new_user("DirEntry", vec![bt.object_.clone()], dict)
            .expect("DirEntry type");
        *slot.borrow_mut() = Some(cls.clone());
        cls
    })
}

fn build_dir_entry(
    name: Object,
    path: Object,
    fs_path: String,
    cached_inode: Option<i64>,
) -> Object {
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
        // `inode()` — the entry's inode number (CPython `DirEntry.inode`),
        // taken from the cached readdir value so it survives the entry being
        // unlinked; falls back to a live `lstat` only when no cache exists.
        let p_ino = fs_path.clone();
        d.insert(
            DictKey(Object::from_static("inode")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "inode",
                binds_instance: false,
                call: Box::new(move |_args| {
                    Ok(Object::Int(
                        cached_inode.unwrap_or_else(|| dir_entry_inode(&p_ino)),
                    ))
                }),
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

/// Read every entry of the directory referred to by `fd` as `(name, inode)`
/// pairs (`.`/`..` filtered out), the shared engine behind `os.scandir(fd)`
/// and `os.listdir(fd)`. The DIR* stream gets its own `dup` (which `closedir`
/// reclaims) so the caller's fd survives — exactly like CPython.
#[cfg(unix)]
fn readdir_entries_fd(fd: libc::c_int) -> Result<Vec<(String, i64)>, RuntimeError> {
    // `fdopendir` takes ownership of the fd it is handed and `closedir` closes
    // it; dup first so the caller's fd survives (CPython dups for this reason).
    let dup_fd = unsafe { libc::dup(fd) };
    if dup_fd < 0 {
        return Err(crate::error::io_error_to_py(
            &std::io::Error::last_os_error(),
        ));
    }
    let dirp = unsafe { libc::fdopendir(dup_fd) };
    if dirp.is_null() {
        let e = std::io::Error::last_os_error();
        unsafe { libc::close(dup_fd) };
        return Err(crate::error::io_error_to_py(&e));
    }
    // `dup(2)` shares the open file description — and thus the directory read
    // position — with the caller's fd, so a second `scandir(fd)` on the same
    // descriptor would start at EOF. Rewind to the start so each call yields
    // the full listing (the shared position is reset to 0, which is harmless
    // for the `openat`-relative descent `rmtree` performs next).
    unsafe { libc::rewinddir(dirp) };
    let mut out: Vec<(String, i64)> = Vec::new();
    loop {
        let ent = unsafe { libc::readdir(dirp) };
        if ent.is_null() {
            break;
        }
        // SAFETY: `readdir` returned a live entry; `d_name` is NUL-terminated.
        let name = unsafe { std::ffi::CStr::from_ptr((*ent).d_name.as_ptr()) };
        let bytes = name.to_bytes();
        if bytes == b"." || bytes == b".." {
            continue;
        }
        let name_str = String::from_utf8_lossy(bytes).into_owned();
        // Cache the inode from the directory read so `DirEntry.inode()` keeps
        // working after the entry is unlinked (the `rmtree` case).
        let ino = unsafe { (*ent).d_ino } as i64;
        out.push((name_str, ino));
    }
    unsafe { libc::closedir(dirp) };
    Ok(out)
}

/// `os.scandir(fd)` — list a directory referred to by an open descriptor.
/// The materialised entries `fstatat` against the *original* fd, exactly like
/// CPython's `DirEntry` (which stores the passed `dir_fd` and relies on the
/// caller keeping it open across the entries' lazy `stat`/`is_dir`).
#[cfg(unix)]
fn scandir_fd(fd: libc::c_int) -> Result<Object, RuntimeError> {
    let entries: Vec<Object> = readdir_entries_fd(fd)?
        .into_iter()
        .map(|(name, ino)| build_dir_entry_fd(name, fd, Some(ino)))
        .collect();
    Ok(build_scandir_iterator(entries))
}

/// `os.listdir(fd)` — the bare entry names (always `str`) of the directory
/// referred to by an open descriptor (RFC 0040 WS1; `os.listdir in
/// os.supports_fd`). CPython `fsdecode`s the names; we use the lossy form.
#[cfg(unix)]
fn listdir_fd(fd: libc::c_int) -> Result<Object, RuntimeError> {
    let names: Vec<Object> = readdir_entries_fd(fd)?
        .into_iter()
        .map(|(name, _)| Object::from_str(name))
        .collect();
    Ok(Object::new_list(names))
}

/// `fstatat(dir_fd, name, follow_symlinks)` → raw `libc::stat`, the engine
/// behind the `os.scandir(fd)` entries' lazy `stat`/`is_*`/`inode`.
#[cfg(unix)]
fn fstatat_raw(dir_fd: libc::c_int, name: &str, follow: bool) -> std::io::Result<libc::stat> {
    let cpath = std::ffi::CString::new(name.as_bytes())
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "embedded null byte"))?;
    let flags = if follow { 0 } else { libc::AT_SYMLINK_NOFOLLOW };
    // SAFETY: `st` is fully written by a successful `fstatat`; `cpath` is a
    // NUL-terminated buffer that is only read.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::fstatat(dir_fd, cpath.as_ptr(), &raw mut st, flags) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(st)
}

/// `st_mode` of an `fstatat`-relative entry (helper for the `is_*` predicates).
#[cfg(unix)]
fn fstatat_mode(dir_fd: libc::c_int, name: &str, follow: bool) -> std::io::Result<libc::mode_t> {
    fstatat_raw(dir_fd, name, follow).map(|st| st.st_mode)
}

/// The `fstatat`-relative twin of [`dir_entry_typecheck`] for `os.scandir(fd)`
/// entries: `is_dir`/`is_file` resolved against the parent's descriptor,
/// `follow_symlinks`-aware (default `True`, matching CPython).
#[cfg(unix)]
fn dir_entry_fd_typecheck(
    name: &'static str,
    dir_fd: libc::c_int,
    ent: String,
    want_dir: bool,
) -> Object {
    let ent_pos = ent.clone();
    let classify = move |ent: &str, follow: bool| -> bool {
        fstatat_mode(dir_fd, ent, follow)
            .map(|m| {
                let fmt = m & libc::S_IFMT;
                if want_dir {
                    fmt == libc::S_IFDIR
                } else {
                    fmt == libc::S_IFREG
                }
            })
            .unwrap_or(false)
    };
    let classify_pos = classify;
    Object::Builtin(Rc::new(crate::object::BuiltinFn {
        name,
        binds_instance: false,
        call: Box::new(move |_args| Ok(Object::Bool(classify_pos(&ent_pos, true)))),
        call_kw: Some(Box::new(move |_args, kwargs| {
            Ok(Object::Bool(classify(&ent, dir_entry_follow(kwargs))))
        })),
    }))
}

/// Build an `os.DirEntry` for an `os.scandir(fd)` listing: `name`/`path` are
/// the bare entry name (no directory to join) and every lazy accessor resolves
/// `fstatat`-relative to `dir_fd` (RFC 0040 WS1).
#[cfg(unix)]
fn build_dir_entry_fd(name: String, dir_fd: libc::c_int, cached_inode: Option<i64>) -> Object {
    use crate::object::BuiltinFn;
    use crate::types::PyInstance;
    let class = dir_entry_type();
    let inst = PyInstance::new(class);
    let name_obj = Object::from_str(name.clone());
    {
        let mut d = inst.dict.borrow_mut();
        d.insert(DictKey(Object::from_static("name")), name_obj.clone());
        // For an fd-relative scandir CPython sets `.path` to the bare entry name
        // (there is no directory path to join onto).
        d.insert(DictKey(Object::from_static("path")), name_obj.clone());
        let fspath = name_obj;
        d.insert(
            DictKey(Object::from_static("__fspath__")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "__fspath__",
                binds_instance: false,
                call: Box::new(move |_args| Ok(fspath.clone())),
                call_kw: None,
            })),
        );
        d.insert(
            DictKey(Object::from_static("is_dir")),
            dir_entry_fd_typecheck("is_dir", dir_fd, name.clone(), true),
        );
        d.insert(
            DictKey(Object::from_static("is_file")),
            dir_entry_fd_typecheck("is_file", dir_fd, name.clone(), false),
        );
        let sym_name = name.clone();
        d.insert(
            DictKey(Object::from_static("is_symlink")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "is_symlink",
                binds_instance: false,
                call: Box::new(move |_args| {
                    Ok(Object::Bool(
                        fstatat_mode(dir_fd, &sym_name, false)
                            .map(|m| (m & libc::S_IFMT) == libc::S_IFLNK)
                            .unwrap_or(false),
                    ))
                }),
                call_kw: None,
            })),
        );
        d.insert(
            DictKey(Object::from_static("is_junction")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "is_junction",
                binds_instance: false,
                call: Box::new(move |_args| Ok(Object::Bool(false))),
                call_kw: None,
            })),
        );
        let ino_name = name.clone();
        d.insert(
            DictKey(Object::from_static("inode")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "inode",
                binds_instance: false,
                call: Box::new(move |_args| {
                    Ok(Object::Int(cached_inode.unwrap_or_else(|| {
                        fstatat_raw(dir_fd, &ino_name, false)
                            .map(|st| st.st_ino as i64)
                            .unwrap_or(0)
                    })))
                }),
                call_kw: None,
            })),
        );
        let stat_name_pos = name.clone();
        let stat_name_kw = name;
        d.insert(
            DictKey(Object::from_static("stat")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "stat",
                binds_instance: false,
                call: Box::new(move |_args| {
                    fstatat_stat_result(dir_fd, &stat_name_pos, true, None)
                }),
                call_kw: Some(Box::new(move |_args, kwargs| {
                    fstatat_stat_result(dir_fd, &stat_name_kw, dir_entry_follow(kwargs), None)
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
    .map_err(|e| crate::error::io_error_to_py_named(&e, Some(fs_path)))?;
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
    // A process-directed signal to *our own* process is, in CPython's
    // single-threaded model, delivered to the main thread (the main
    // thread *is* the process). WeavePy runs the interpreter on a
    // dedicated VM thread while the process's initial OS thread parks
    // with async signals blocked; a self-directed `kill` issued while
    // the VM thread has `sig` blocked can otherwise be absorbed by the
    // parked thread's per-thread pending queue, invisible to
    // `sigpending()` and never delivered (`test_signal`
    // test_pthread_sigmask / test_sigpending). Route it onto the VM main
    // thread via `pthread_kill` to reproduce the single-threaded
    // semantics. Real process groups (`pid <= 0`) and other pids take
    // the genuine `kill` path.
    if pid == unsafe { libc::getpid() } && sig != 0 {
        if let Some(rc) = crate::stdlib::signal_mod::deliver_to_vm_main(sig as i32) {
            if rc != 0 {
                return Err(crate::error::io_error_to_py(
                    &std::io::Error::from_raw_os_error(rc),
                ));
            }
            return Ok(Object::None);
        }
    }
    let rc = unsafe { libc::kill(pid, sig) };
    if rc != 0 {
        return Err(crate::error::io_error_to_py(
            &std::io::Error::last_os_error(),
        ));
    }
    Ok(Object::None)
}

/// Windows `os.kill` — CPython's `os_kill_impl` under `MS_WINDOWS`: the two
/// console-control "signals" (`CTRL_C_EVENT`/`CTRL_BREAK_EVENT`) route to
/// `GenerateConsoleCtrlEvent(sig, pid)`; anything else terminates the target
/// via `OpenProcess` + `TerminateProcess(handle, sig)`.
#[cfg(windows)]
fn os_kill(args: &[Object]) -> Result<Object, RuntimeError> {
    use crate::stdlib::nt_support::last_win32_error_to_py;
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Console::{
        GenerateConsoleCtrlEvent, CTRL_BREAK_EVENT, CTRL_C_EVENT,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, TerminateProcess, PROCESS_ALL_ACCESS,
    };
    let pid = match args.first().and_then(Object::as_i64) {
        Some(p) => p,
        None => return Err(type_error("kill() pid must be int")),
    };
    let sig = match args.get(1).and_then(Object::as_i64) {
        Some(s) => s,
        None => return Err(type_error("kill() signal must be int")),
    };
    if sig == i64::from(CTRL_C_EVENT) || sig == i64::from(CTRL_BREAK_EVENT) {
        if unsafe { GenerateConsoleCtrlEvent(sig as u32, pid as u32) } == 0 {
            return Err(last_win32_error_to_py(None));
        }
        return Ok(Object::None);
    }
    // SAFETY: plain Win32 calls; the handle is closed on every path.
    let handle = unsafe { OpenProcess(PROCESS_ALL_ACCESS, 0, pid as u32) };
    if handle.is_null() {
        return Err(last_win32_error_to_py(None));
    }
    let ok = unsafe { TerminateProcess(handle, sig as u32) };
    let err = if ok == 0 {
        Some(last_win32_error_to_py(None))
    } else {
        None
    };
    unsafe { CloseHandle(handle) };
    match err {
        Some(e) => Err(e),
        None => Ok(Object::None),
    }
}

#[cfg(not(any(unix, windows)))]
fn os_kill(_args: &[Object]) -> Result<Object, RuntimeError> {
    Err(crate::error::not_implemented_error(
        "os.kill() is only implemented on POSIX in WeavePy",
    ))
}

/// `os.system(command)` — run `command` through the shell via libc
/// `system(3)`. Returns the raw `wait()`-encoded status on POSIX
/// (callers decode with `os.waitstatus_to_exitcode`), matching
/// CPython's `posix.system`.
#[cfg(unix)]
fn os_system(args: &[Object]) -> Result<Object, RuntimeError> {
    let command = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        Some(Object::Bytes(b)) => String::from_utf8_lossy(b).into_owned(),
        _ => {
            return Err(type_error(
                "system() argument must be str or bytes, not None",
            ))
        }
    };
    let c_command = std::ffi::CString::new(command)
        .map_err(|_| crate::error::value_error("embedded null byte"))?;
    // Release the GIL: the child shell can run arbitrarily long and
    // may itself be a WeavePy re-invocation that needs the lock.
    let status = crate::gil::allow_threads_then(|| unsafe { libc::system(c_command.as_ptr()) });
    Ok(Object::Int(i64::from(status)))
}

/// Windows `os.system` — CPython calls the CRT's wide `_wsystem` and
/// returns its result (the `cmd.exe` exit code) directly.
#[cfg(windows)]
fn os_system(args: &[Object]) -> Result<Object, RuntimeError> {
    // Not part of nt_support's audited CRT block (os.system is the only
    // consumer); declared here like CPython keeps `_wsystem` local to
    // posixmodule.c.
    unsafe extern "C" {
        fn _wsystem(command: *const u16) -> i32;
    }
    let command = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        Some(Object::Bytes(b)) => String::from_utf8_lossy(b).into_owned(),
        _ => {
            return Err(type_error(
                "system() argument must be str or bytes, not None",
            ))
        }
    };
    if command.as_bytes().contains(&0) {
        return Err(crate::error::value_error("embedded null byte"));
    }
    let wcmd = crate::stdlib::nt_support::wide(&command);
    // Release the GIL: the child shell can run arbitrarily long and may
    // itself be a WeavePy re-invocation that needs the lock.
    let status = crate::gil::allow_threads_then(|| unsafe { _wsystem(wcmd.as_ptr()) });
    Ok(Object::Int(i64::from(status)))
}

#[cfg(not(any(unix, windows)))]
fn os_system(_args: &[Object]) -> Result<Object, RuntimeError> {
    Err(crate::error::not_implemented_error(
        "os.system() is only implemented on POSIX in WeavePy",
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

/// Windows `os.waitpid` — the `pid` is a process *handle* returned by
/// `os.spawnv(P_NOWAIT, …)`, and the wait is the CRT's `_cwait`
/// (posixmodule.c `os_waitpid_impl` under `MS_WINDOWS`). The returned
/// status is the exit code shifted left 8 bits, so the portable
/// `os.waitstatus_to_exitcode(status)` (`status >> 8`) recovers it.
#[cfg(windows)]
fn os_waitpid(args: &[Object]) -> Result<Object, RuntimeError> {
    unsafe extern "C" {
        fn _cwait(termstat: *mut i32, prochandle: isize, action: i32) -> isize;
    }
    let pid = match args.first() {
        Some(Object::Int(p)) => *p,
        _ => return Err(type_error("waitpid() pid must be int")),
    };
    let options = match args.get(1) {
        Some(Object::Int(o)) => *o as i32,
        Some(Object::None) | None => 0,
        _ => return Err(type_error("waitpid() options must be int")),
    };
    let mut status: i32 = 0;
    let status_ptr: *mut i32 = &raw mut status;
    // Release the GIL across the blocking wait, mirroring the Unix arm
    // (`_cwait` ignores `action`, but CPython passes it through too).
    let rc =
        crate::gil::allow_threads_then(|| unsafe { _cwait(status_ptr, pid as isize, options) });
    if rc == -1 {
        return Err(crate::stdlib::nt_support::last_crt_error_to_py(None));
    }
    Ok(Object::new_tuple(vec![
        Object::Int(rc as i64),
        Object::Int(i64::from(status) << 8),
    ]))
}

#[cfg(not(any(unix, windows)))]
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

/// Windows `os.waitstatus_to_exitcode` — the inverse of `os.waitpid`'s
/// `<< 8` encoding: CPython's Windows arm is simply `status >> 8`.
#[cfg(windows)]
fn os_waitstatus_to_exitcode(args: &[Object]) -> Result<Object, RuntimeError> {
    let status = match args.first() {
        Some(Object::Int(s)) => *s,
        Some(Object::Bool(b)) => i64::from(*b),
        _ => return Err(type_error("an integer is required")),
    };
    Ok(Object::Int(status >> 8))
}

#[cfg(not(any(unix, windows)))]
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

// No non-Unix arm: the registration is `#[cfg(unix)]` (CPython's `nt` has no
// `set_blocking`/`get_blocking` — `O_NONBLOCK` has no CRT-fd analogue).

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
    // Windows: CPython's `os_pipe_impl` — an anonymous pipe from
    // `CreatePipe` (NULL security attributes ⇒ non-inheritable handles,
    // PEP 446), each end adopted into a CRT fd with `O_NOINHERIT`.
    #[cfg(windows)]
    {
        use crate::stdlib::nt_support::{crt, last_crt_error_to_py, last_win32_error_to_py};
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Pipes::CreatePipe;
        let mut read: *mut std::ffi::c_void = std::ptr::null_mut();
        let mut write: *mut std::ffi::c_void = std::ptr::null_mut();
        // SAFETY: plain Win32/CRT calls; on every failure path the handles
        // that haven't been adopted by a CRT fd are closed exactly once.
        unsafe {
            if CreatePipe(&raw mut read, &raw mut write, std::ptr::null(), 0) == 0 {
                return Err(last_win32_error_to_py(None));
            }
            let rfd = crt::_open_osfhandle(read as crt::intptr_t, crt::O_RDONLY | crt::O_NOINHERIT);
            if rfd < 0 {
                let e = last_crt_error_to_py(None);
                CloseHandle(read);
                CloseHandle(write);
                return Err(e);
            }
            let wfd =
                crt::_open_osfhandle(write as crt::intptr_t, crt::O_WRONLY | crt::O_NOINHERIT);
            if wfd < 0 {
                let e = last_crt_error_to_py(None);
                crt::_close(rfd); // closes `read` (the fd owns it)
                CloseHandle(write);
                return Err(e);
            }
            Ok(Object::new_tuple(vec![
                Object::Int(i64::from(rfd)),
                Object::Int(i64::from(wfd)),
            ]))
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        Err(crate::error::not_implemented_error(
            "os.pipe() is only implemented on POSIX in WeavePy",
        ))
    }
}

// POSIX-only (no pty on NT); the registration is `#[cfg(unix)]`.
#[cfg(unix)]
fn os_openpty(_args: &[Object]) -> Result<Object, RuntimeError> {
    {
        let mut master: libc::c_int = -1;
        let mut slave: libc::c_int = -1;
        let rc = unsafe {
            libc::openpty(
                &raw mut master,
                &raw mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if rc != 0 {
            return Err(crate::error::os_error("openpty() failed"));
        }
        // PEP 446: both descriptors are non-inheritable.
        unsafe {
            for fd in [master, slave] {
                let flags = libc::fcntl(fd, libc::F_GETFD);
                if flags >= 0 {
                    libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
                }
            }
        }
        Ok(Object::new_tuple(vec![
            Object::Int(i64::from(master)),
            Object::Int(i64::from(slave)),
        ]))
    }
}

/// `os.login_tty(fd)` — make `fd` the controlling terminal and the new
/// stdin/stdout/stderr (CPython 3.11+, `HAVE_LOGIN_TTY`). The frozen
/// `pty.fork()` calls this in the forked child.
#[cfg(unix)]
#[allow(clippy::cast_lossless)] // ioctl's request type is c_ulong or c_int per libc
fn os_login_tty(args: &[Object]) -> Result<Object, RuntimeError> {
    let fd = match args.first() {
        Some(Object::Int(i)) => *i as libc::c_int,
        Some(Object::Bool(b)) => libc::c_int::from(*b),
        _ => return Err(type_error("login_tty() arg must be int")),
    };
    // BSD `login_tty(3)` semantics, spelled out so we don't depend on a
    // libutil symbol that not every libc build exports: new session, make
    // `fd` the controlling terminal, then splat it over the stdio fds.
    unsafe {
        libc::setsid();
        if libc::ioctl(fd, libc::TIOCSCTTY as _, 0 as libc::c_long) == -1 {
            return Err(crate::error::io_error_to_py(
                &std::io::Error::last_os_error(),
            ));
        }
        for std_fd in 0..3 {
            if libc::dup2(fd, std_fd) == -1 {
                return Err(crate::error::io_error_to_py(
                    &std::io::Error::last_os_error(),
                ));
            }
        }
        if fd > 2 {
            libc::close(fd);
        }
    }
    Ok(Object::None)
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
            // Preserve the real errno (`EBADF` for a closed/invalid fd) so
            // `os.dup(bad).errno == errno.EBADF` — `test_os.TestInvalidFD`.
            return Err(crate::error::io_error_to_py(
                &std::io::Error::last_os_error(),
            ));
        }
        // PEP 446: `os.dup` returns a *non-inheritable* descriptor (FD_CLOEXEC
        // set), unlike the raw `dup(2)`. `test_os.FDInheritanceTests.test_dup`
        // asserts `os.get_inheritable(dup_fd) is False`.
        unsafe {
            let flags = libc::fcntl(new, libc::F_GETFD);
            if flags >= 0 {
                libc::fcntl(new, libc::F_SETFD, flags | libc::FD_CLOEXEC);
            }
        }
        Ok(Object::Int(i64::from(new)))
    }
    // Windows: CRT `_dup`, then clear the duplicate handle's inheritance
    // flag — CPython's `os.dup` goes through `_Py_dup`, which makes the new
    // descriptor non-inheritable (PEP 446). `_dup` itself duplicates the
    // handle *inheritable*, so the explicit clear is load-bearing.
    #[cfg(windows)]
    {
        use crate::stdlib::nt_support::{crt, last_crt_error_to_py, last_win32_error_to_py};
        use windows_sys::Win32::Foundation::{SetHandleInformation, HANDLE_FLAG_INHERIT};
        let new = unsafe { crt::_dup(fd) };
        if new < 0 {
            return Err(last_crt_error_to_py(None));
        }
        let handle = unsafe { crt::_get_osfhandle(new) };
        if unsafe { SetHandleInformation(handle as *mut std::ffi::c_void, HANDLE_FLAG_INHERIT, 0) }
            == 0
        {
            return Err(last_win32_error_to_py(None));
        }
        Ok(Object::Int(i64::from(new)))
    }
    #[cfg(not(any(unix, windows)))]
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
    let inheritable = match args.get(2).or_else(|| {
        kwargs
            .iter()
            .find(|(k, _)| k == "inheritable")
            .map(|(_, v)| v)
    }) {
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
    // Windows: CRT `_dup2` (0 on success), then set the target handle's
    // inheritance to match `inheritable` — CPython's `os_dup2_impl` calls
    // `_Py_set_inheritable` on `fd2` after the dup.
    #[cfg(windows)]
    {
        use crate::stdlib::nt_support::{crt, last_crt_error_to_py, last_win32_error_to_py};
        use windows_sys::Win32::Foundation::{SetHandleInformation, HANDLE_FLAG_INHERIT};
        let rc = unsafe { crt::_dup2(fd, newfd) };
        if rc != 0 {
            return Err(last_crt_error_to_py(None));
        }
        let handle = unsafe { crt::_get_osfhandle(newfd) };
        let flag = if inheritable { HANDLE_FLAG_INHERIT } else { 0 };
        if unsafe { SetHandleInformation(handle as _, HANDLE_FLAG_INHERIT, flag) } == 0 {
            return Err(last_win32_error_to_py(None));
        }
        Ok(Object::Int(i64::from(newfd)))
    }
    #[cfg(not(any(unix, windows)))]
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
    // Windows: the CRT's 64-bit seek (`_lseeki64`), CPython's `os_lseek_impl`.
    #[cfg(windows)]
    {
        use crate::stdlib::nt_support::{crt, last_crt_error_to_py};
        let off = unsafe { crt::_lseeki64(fd, pos, how) };
        if off < 0 {
            return Err(last_crt_error_to_py(None));
        }
        Ok(Object::Int(off))
    }
    #[cfg(not(any(unix, windows)))]
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
/// `os.truncate(path, length)` — truncate a file given a path (str/bytes/
/// `PathLike`) or an open fd (int). The fd form is exactly `os.ftruncate`.
fn os_truncate(args: &[Object]) -> Result<Object, RuntimeError> {
    // An int first argument is a file descriptor → delegate to `ftruncate`.
    if matches!(args.first(), Some(Object::Int(_) | Object::Bool(_))) {
        return os_ftruncate(args);
    }
    let length = match args.get(1) {
        Some(Object::Int(i)) => *i,
        Some(Object::Bool(b)) => i64::from(*b),
        _ => return Err(type_error("truncate() length must be int")),
    };
    if length < 0 {
        return Err(crate::error::io_error_to_py(
            &std::io::Error::from_raw_os_error(22), // EINVAL
        ));
    }
    let p = first_path(args, "truncate")?;
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let cpath = std::ffi::CString::new(std::ffi::OsStr::new(&p).as_bytes())
            .map_err(|_| value_error("embedded null character in path"))?;
        let rc = unsafe { libc::truncate(cpath.as_ptr(), length as libc::off_t) };
        if rc != 0 {
            return Err(path_io_err(
                &std::io::Error::last_os_error(),
                args.first(),
                &p,
            ));
        }
        Ok(Object::None)
    }
    // Windows has no path `truncate(2)`: CPython opens the file write-only
    // and sizes it with `_chsize_s` (posixmodule.c `os_truncate_impl` under
    // `MS_WINDOWS`).
    #[cfg(windows)]
    {
        use crate::stdlib::nt_support::{crt, crt_error_to_py, wide};
        if p.as_bytes().contains(&0) {
            return Err(value_error("embedded null byte"));
        }
        let wpath = wide(&p);
        let mut fd: i32 = -1;
        let err = unsafe {
            crt::_wsopen_s(
                &raw mut fd,
                wpath.as_ptr(),
                crt::O_WRONLY | crt::O_BINARY | crt::O_NOINHERIT,
                crt::SH_DENYNO,
                0,
            )
        };
        if err != 0 {
            return Err(crt_error_to_py(err, Some(&p)));
        }
        let rc = unsafe { crt::_chsize_s(fd, length) };
        unsafe { crt::_close(fd) };
        if rc != 0 {
            return Err(crt_error_to_py(rc, Some(&p)));
        }
        Ok(Object::None)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (p, length);
        Err(crate::error::not_implemented_error(
            "os.truncate() is only implemented on POSIX in WeavePy",
        ))
    }
}

fn os_ftruncate(args: &[Object]) -> Result<Object, RuntimeError> {
    let fd = match args.first() {
        Some(Object::Int(i)) => *i as i32,
        Some(Object::Bool(b)) => {
            // CPython 3.12+: a `bool` where an fd is expected raises a
            // `RuntimeWarning` (`test_os.TestInvalidFD.test_ftruncate` runs
            // under `simplefilter("error", RuntimeWarning)`).
            warn_bool_as_fd()?;
            i32::from(*b)
        }
        _ => return Err(type_error("ftruncate() fd must be int")),
    };
    let length = match args.get(1) {
        Some(Object::Int(i)) => *i,
        Some(Object::Bool(b)) => i64::from(*b),
        _ => return Err(type_error("ftruncate() length must be int")),
    };
    if length < 0 {
        return Err(crate::error::io_error_to_py(
            &std::io::Error::from_raw_os_error(
                22, // EINVAL
            ),
        ));
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
    // Windows: `_chsize_s` (CPython's `os_ftruncate_impl`); it returns the
    // errno directly rather than setting the TLS `errno`.
    #[cfg(windows)]
    {
        use crate::stdlib::nt_support::{crt, crt_error_to_py};
        let rc = unsafe { crt::_chsize_s(fd, length) };
        if rc != 0 {
            return Err(crt_error_to_py(rc, None));
        }
        Ok(Object::None)
    }
    #[cfg(not(any(unix, windows)))]
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
    // Windows: inheritance lives on the *handle* — CPython's
    // `_Py_get_inheritable` reads `GetHandleInformation`'s
    // `HANDLE_FLAG_INHERIT` bit for the fd's OS handle.
    #[cfg(windows)]
    {
        use crate::stdlib::nt_support::{crt, crt_error_to_py, last_win32_error_to_py};
        use windows_sys::Win32::Foundation::{GetHandleInformation, HANDLE_FLAG_INHERIT};
        let handle = unsafe { crt::_get_osfhandle(fd) };
        if handle == -1 || handle == -2 {
            return Err(crt_error_to_py(crate::py_errno::EBADF, None));
        }
        let mut flags: u32 = 0;
        if unsafe { GetHandleInformation(handle as _, &raw mut flags) } == 0 {
            return Err(last_win32_error_to_py(None));
        }
        Ok(Object::Bool(flags & HANDLE_FLAG_INHERIT != 0))
    }
    #[cfg(not(any(unix, windows)))]
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
    // Windows: `SetHandleInformation(HANDLE_FLAG_INHERIT, …)` on the fd's
    // handle (CPython's `_Py_set_inheritable`).
    #[cfg(windows)]
    {
        use crate::stdlib::nt_support::{crt, crt_error_to_py, last_win32_error_to_py};
        use windows_sys::Win32::Foundation::{SetHandleInformation, HANDLE_FLAG_INHERIT};
        let handle = unsafe { crt::_get_osfhandle(fd) };
        if handle == -1 || handle == -2 {
            return Err(crt_error_to_py(crate::py_errno::EBADF, None));
        }
        let flag = if inheritable { HANDLE_FLAG_INHERIT } else { 0 };
        if unsafe { SetHandleInformation(handle as _, HANDLE_FLAG_INHERIT, flag) } == 0 {
            return Err(last_win32_error_to_py(None));
        }
        Ok(Object::None)
    }
    #[cfg(not(any(unix, windows)))]
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
    // Windows: the CRT's `_isatty` (true for any character device — console,
    // NUL — exactly like CPython's `os_isatty_impl`).
    #[cfg(windows)]
    {
        let r = unsafe { crate::stdlib::nt_support::crt::_isatty(fd as i32) };
        Ok(Object::Bool(r != 0))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = fd;
        Ok(Object::Bool(false))
    }
}

/// `os.device_encoding(fd)` — the encoding of the terminal attached to
/// `fd`, or `None` when `fd` is not a tty. On POSIX CPython returns the
/// locale codeset (`nl_langinfo(CODESET)`); we do the same so a tty fd
/// reports e.g. `"UTF-8"` and a pipe/file reports `None`.
fn os_device_encoding(args: &[Object]) -> Result<Object, RuntimeError> {
    let fd = match args.first() {
        Some(Object::Int(i)) => *i as i32,
        Some(Object::Bool(b)) => i32::from(*b),
        _ => return Err(type_error("device_encoding() arg must be int")),
    };
    #[cfg(unix)]
    {
        if unsafe { libc::isatty(fd) } == 0 {
            return Ok(Object::None);
        }
        let codeset = unsafe {
            let p = libc::nl_langinfo(libc::CODESET);
            if p.is_null() {
                String::new()
            } else {
                std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned()
            }
        };
        // An empty/unset locale codeset still implies the C locale's
        // default; CPython falls back to UTF-8 on macOS, ASCII on Linux's
        // "C" locale. Use UTF-8 as the portable default rather than "".
        if codeset.is_empty() {
            Ok(Object::from_static("UTF-8"))
        } else {
            Ok(Object::from_str(codeset))
        }
    }
    // Windows: CPython's `_Py_device_encoding` — `None` for a non-tty; for a
    // console fd, `'cp%d'` of the input code page on fd 0 and the output
    // code page on fds 1/2.
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Console::{GetConsoleCP, GetConsoleOutputCP};
        if unsafe { crate::stdlib::nt_support::crt::_isatty(fd) } == 0 {
            return Ok(Object::None);
        }
        let cp = match fd {
            0 => unsafe { GetConsoleCP() },
            1 | 2 => unsafe { GetConsoleOutputCP() },
            _ => 0,
        };
        if cp == 0 {
            return Ok(Object::None);
        }
        Ok(Object::from_str(format!("cp{cp}")))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = fd;
        Ok(Object::None)
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
            let r = crate::gil::allow_threads_then(|| unsafe { libc::read(fd, ptr.cast(), n) });
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
    // Windows: the CRT's `_read` on the fd (CPython's `os_read_impl` →
    // `_Py_read`). The count parameter is 32-bit, so clamp a larger request
    // like `_PY_READ_MAX`; a short read is normal and the caller loops.
    #[cfg(windows)]
    {
        let mut buf = vec![0u8; n];
        let want = u32::try_from(n.min(i32::MAX as usize)).expect("clamped to i32::MAX");
        let ptr = buf.as_mut_ptr();
        // Release the GIL like the Unix arm: a pipe read can block
        // indefinitely and peer threads must keep running.
        let r = crate::gil::allow_threads_then(|| unsafe {
            crate::stdlib::nt_support::crt::_read(fd, ptr.cast(), want)
        });
        if r < 0 {
            return Err(crate::stdlib::nt_support::last_crt_error_to_py(None));
        }
        buf.truncate(r as usize);
        Ok(Object::new_bytes(buf))
    }
    #[cfg(not(any(unix, windows)))]
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
        // CPython's `os.write` accepts *any* buffer-protocol object and rejects
        // only non-buffers like `str` (`test_os.FileTests.test_write`). Route
        // `array.array`, PEP 688 `__buffer__` exporters, and bytes/bytearray
        // subclasses through the shared buffer-view extractor
        // (`test_io.test_array_writes`).
        Some(other) => crate::builtins::bytes_argview(other)?,
        None => return Err(type_error("write() takes exactly 2 positional arguments")),
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
    // Windows: the CRT's `_write` (CPython's `os_write_impl` → `_Py_write`).
    // 32-bit count: a longer buffer is written partially and the caller's
    // write loop (io, subprocess) resumes from the returned count.
    #[cfg(windows)]
    {
        let want = u32::try_from(data.len().min(i32::MAX as usize)).expect("clamped to i32::MAX");
        let r = crate::gil::allow_threads_then(|| unsafe {
            crate::stdlib::nt_support::crt::_write(fd, data.as_ptr().cast(), want)
        });
        if r < 0 {
            return Err(crate::stdlib::nt_support::last_crt_error_to_py(None));
        }
        Ok(Object::Int(i64::from(r)))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (fd, data);
        Err(crate::error::not_implemented_error(
            "os.write() is only implemented on POSIX in WeavePy",
        ))
    }
}

/// `os.sendfile(out_fd, in_fd, offset, count, [headers, trailers, flags])` —
/// zero-copy file-to-socket transfer via `sendfile(2)` (RFC 0068 WS8). Backs
/// `socket.socket.sendfile`'s `_sendfile_use_sendfile` path and asyncio's
/// `_sock_sendfile_native`, both of which activate on `hasattr(os,
/// 'sendfile')`.
///
/// The platform shapes follow CPython's `os_sendfile_impl` exactly:
///
/// - **macOS**: `sendfile(in_fd, out_fd, offset, &len, &sf_hdtr, flags)`.
///   `len` is in-out — on entry the byte budget (`count` *plus* every
///   header's length, matching CPython's Apple-only adjustment; `0` means
///   send to EOF), on exit the total actually sent, headers/trailers
///   included, which is the return value. `headers`/`trailers` are
///   sequences of buffers gathered through `struct sf_hdtr`. An
///   `EAGAIN`/`EBUSY` after a partial transfer returns the partial count;
///   with nothing sent it raises (`EAGAIN` maps to `BlockingIOError`, which
///   `socket.py`'s selector retry loop catches).
/// - **Linux**: `sendfile(out_fd, in_fd, &offset|NULL, count)` — no
///   header/trailer support; `offset=None` uses (and advances) `in_fd`'s
///   own file position; returns the syscall's byte count.
#[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
fn os_sendfile(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let get = |idx: usize, name: &str| -> Option<&Object> {
        args.get(idx)
            .or_else(|| kwargs.iter().find(|(k, _)| k == name).map(|(_, v)| v))
    };
    let int_arg = |o: Option<&Object>, name: &str| -> Result<i64, RuntimeError> {
        match o {
            Some(Object::Int(i)) => Ok(*i),
            Some(Object::Bool(b)) => Ok(i64::from(*b)),
            _ => Err(type_error(format!(
                "sendfile() argument '{name}' must be an int"
            ))),
        }
    };
    #[cfg(target_os = "macos")]
    {
        for (k, _) in kwargs {
            if !matches!(
                k.as_str(),
                "out_fd" | "in_fd" | "offset" | "count" | "headers" | "trailers" | "flags"
            ) {
                return Err(type_error(format!(
                    "'{k}' is an invalid keyword argument for sendfile()"
                )));
            }
        }
        let out_fd = int_arg(get(0, "out_fd"), "out_fd")? as i32;
        let in_fd = int_arg(get(1, "in_fd"), "in_fd")? as i32;
        let offset = int_arg(get(2, "offset"), "offset")?;
        let count = int_arg(get(3, "count"), "count")?;
        let flags = match get(6, "flags") {
            None => 0,
            some => int_arg(some, "flags")? as i32,
        };
        // Materialise each header/trailer buffer up front; the Vecs must
        // outlive the syscall so the iovec base pointers stay valid.
        let gather = |o: Option<&Object>, which: &str| -> Result<Vec<Vec<u8>>, RuntimeError> {
            let Some(o) = o else { return Ok(Vec::new()) };
            let items = match o {
                Object::Tuple(t) => t.to_vec(),
                Object::List(l) => l.borrow().clone(),
                _ => return Err(type_error(format!("sendfile() {which} must be a sequence"))),
            };
            items
                .iter()
                .map(|it| match it {
                    Object::Bytes(b) => Ok(b.to_vec()),
                    Object::ByteArray(b) => Ok(b.borrow().clone()),
                    Object::MemoryView(mv) => Ok(mv.to_bytes()),
                    other => crate::builtins::bytes_argview(other),
                })
                .collect()
        };
        let headers = gather(get(4, "headers"), "headers")?;
        let trailers = gather(get(5, "trailers"), "trailers")?;
        // Apple's `len` budget covers the headers too: CPython adds each
        // header's length to `sbytes` so `count` file bytes still go out.
        let mut sbytes: libc::off_t = count;
        for h in &headers {
            let blen = h.len() as i64;
            if sbytes >= i64::MAX - blen {
                return Err(crate::error::overflow_error(
                    "sendfile() header is too large",
                ));
            }
            sbytes += blen;
        }
        let hdr_iov: Vec<libc::iovec> = headers
            .iter()
            .map(|b| libc::iovec {
                iov_base: b.as_ptr() as *mut _,
                iov_len: b.len(),
            })
            .collect();
        let trl_iov: Vec<libc::iovec> = trailers
            .iter()
            .map(|b| libc::iovec {
                iov_base: b.as_ptr() as *mut _,
                iov_len: b.len(),
            })
            .collect();
        // SAFETY: zeroed `sf_hdtr` is the "no headers/trailers" shape; the
        // iovec arrays outlive the syscall loop below.
        let mut sf: libc::sf_hdtr = unsafe { std::mem::zeroed() };
        if !hdr_iov.is_empty() {
            sf.headers = hdr_iov.as_ptr().cast_mut();
            sf.hdr_cnt = hdr_iov.len() as i32;
        }
        if !trl_iov.is_empty() {
            sf.trailers = trl_iov.as_ptr().cast_mut();
            sf.trl_cnt = trl_iov.len() as i32;
        }
        loop {
            let sb = &mut sbytes;
            let sfp = &mut sf;
            let r = crate::gil::allow_threads_then(|| unsafe {
                libc::sendfile(in_fd, out_fd, offset as libc::off_t, sb, sfp, flags)
            });
            if r < 0 {
                let err = std::io::Error::last_os_error();
                let errno = err.raw_os_error();
                if errno == Some(libc::EINTR) {
                    // PEP 475: run tripped handlers, then retry. The kernel
                    // rewrote `sbytes` to the bytes already sent, and CPython
                    // reuses that as the next call's budget — so do we.
                    service_pending_signals()?;
                    continue;
                }
                if matches!(errno, Some(libc::EAGAIN) | Some(libc::EBUSY)) && sbytes != 0 {
                    // Partial transfer before the socket buffer filled:
                    // CPython reports the partial count as success.
                    return Ok(Object::Int(sbytes));
                }
                return Err(crate::error::io_error_to_py(&err));
            }
            return Ok(Object::Int(sbytes));
        }
    }
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        for (k, _) in kwargs {
            if !matches!(k.as_str(), "out_fd" | "in_fd" | "offset" | "count") {
                return Err(type_error(format!(
                    "'{k}' is an invalid keyword argument for sendfile()"
                )));
            }
        }
        let out_fd = int_arg(get(0, "out_fd"), "out_fd")? as i32;
        let in_fd = int_arg(get(1, "in_fd"), "in_fd")? as i32;
        let offset_obj = get(2, "offset");
        if offset_obj.is_none() {
            return Err(type_error("sendfile() missing required argument 'offset'"));
        }
        let count = int_arg(get(3, "count"), "count")?;
        // `offset=None` → NULL pointer: use and advance the fd's own file
        // position, exactly CPython's Linux branch.
        let use_off = !matches!(offset_obj, Some(Object::None));
        let mut off: libc::off_t = if use_off {
            int_arg(offset_obj, "offset")?
        } else {
            0
        };
        loop {
            let offp = if use_off {
                &raw mut off
            } else {
                std::ptr::null_mut()
            };
            let r = crate::gil::allow_threads_then(|| unsafe {
                libc::sendfile(out_fd, in_fd, offp, count as usize)
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
}

/// `os.uname()` — host identification via `uname(2)`, returned as an
/// `os.uname_result` struct sequence. `platform.uname()`/`platform.mac_ver()`
/// (and thus `@support.requires_mac_ver`, `test_shutil.test_tarfile_vs_tar`)
/// read `.sysname`/`.release`/`.machine`.
#[cfg(unix)]
fn os_uname(args: &[Object]) -> Result<Object, RuntimeError> {
    require_no_args(args, "uname")?;
    // SAFETY: `uname` fills the zeroed `utsname`; we only read it afterwards.
    let mut uts: libc::utsname = unsafe { std::mem::zeroed() };
    if unsafe { libc::uname(&raw mut uts) } != 0 {
        return Err(crate::error::io_error_to_py(
            &std::io::Error::last_os_error(),
        ));
    }
    fn field(arr: &[libc::c_char]) -> Object {
        let bytes: Vec<u8> = arr
            .iter()
            .take_while(|&&c| c != 0)
            .map(|&c| c as u8)
            .collect();
        Object::from_str(String::from_utf8_lossy(&bytes).into_owned())
    }
    Ok(struct_seq_instance(
        uname_result_type(),
        &UNAME_FIELDS,
        vec![
            field(&uts.sysname),
            field(&uts.nodename),
            field(&uts.release),
            field(&uts.version),
            field(&uts.machine),
        ],
    ))
}

/// Field names of `os.times_result` (CPython `Modules/posixmodule.c`).
const TIMES_FIELDS: [&str; 5] = [
    "user",
    "system",
    "children_user",
    "children_system",
    "elapsed",
];

/// Memoised `os.times_result` struct-sequence type (`isinstance` identity).
fn times_result_type() -> Rc<crate::types::TypeObject> {
    struct_seq_type("times_result", "os", &TIMES_FIELDS)
}

/// `os.times()` — process & children CPU times plus wall-clock elapsed, each in
/// seconds, as an `os.times_result` struct sequence (`test_os.TimesTests`).
#[cfg(unix)]
fn os_times(args: &[Object]) -> Result<Object, RuntimeError> {
    require_no_args(args, "times")?;
    // SAFETY: `times(2)` fills the zeroed `tms`; we only read it afterwards.
    let mut buf: libc::tms = unsafe { std::mem::zeroed() };
    let elapsed = unsafe { libc::times(&raw mut buf) };
    let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    let tps = if ticks > 0 { ticks as f64 } else { 100.0 };
    let secs = |t: libc::clock_t| Object::Float(t as f64 / tps);
    Ok(struct_seq_instance(
        times_result_type(),
        &TIMES_FIELDS,
        vec![
            secs(buf.tms_utime),
            secs(buf.tms_stime),
            secs(buf.tms_cutime),
            secs(buf.tms_cstime),
            Object::Float(elapsed as f64 / tps),
        ],
    ))
}

/// Windows `os.times` — CPython's `os_times_impl` under `MS_WINDOWS`:
/// `GetProcessTimes` kernel/user FILETIMEs (100ns units) for `system`/`user`;
/// the children and elapsed slots are 0 (NT doesn't aggregate child times).
#[cfg(windows)]
fn os_times(args: &[Object]) -> Result<Object, RuntimeError> {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    require_no_args(args, "times")?;
    let zero = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let (mut create, mut exit, mut kernel, mut user) = (zero, zero, zero, zero);
    // SAFETY: the pseudo-handle from GetCurrentProcess is always valid.
    let ok = unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &raw mut create,
            &raw mut exit,
            &raw mut kernel,
            &raw mut user,
        )
    };
    if ok == 0 {
        return Err(crate::stdlib::nt_support::last_win32_error_to_py(None));
    }
    let secs = |ft: &FILETIME| {
        let ticks = (u64::from(ft.dwHighDateTime) << 32) | u64::from(ft.dwLowDateTime);
        ticks as f64 * 1e-7
    };
    Ok(struct_seq_instance(
        times_result_type(),
        &TIMES_FIELDS,
        vec![
            Object::Float(secs(&user)),
            Object::Float(secs(&kernel)),
            Object::Float(0.0),
            Object::Float(0.0),
            Object::Float(0.0),
        ],
    ))
}

#[cfg(not(any(unix, windows)))]
fn os_times(args: &[Object]) -> Result<Object, RuntimeError> {
    require_no_args(args, "times")?;
    let zero = || Object::Float(0.0);
    Ok(struct_seq_instance(
        times_result_type(),
        &TIMES_FIELDS,
        vec![zero(), zero(), zero(), zero(), zero()],
    ))
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
    // Windows: `GetConsoleScreenBufferInfo` on the fd's handle, raising
    // `OSError` when it isn't a console (CPython's `os_get_terminal_size_impl`
    // — the frozen `shutil.get_terminal_size` catches that and falls back).
    #[cfg(windows)]
    {
        use crate::stdlib::nt_support::{crt, crt_error_to_py, last_win32_error_to_py};
        use windows_sys::Win32::System::Console::{
            GetConsoleScreenBufferInfo, CONSOLE_SCREEN_BUFFER_INFO,
        };
        let fd = match args.first() {
            Some(Object::Int(n)) => *n as i32,
            Some(Object::Bool(b)) => i32::from(*b),
            None | Some(Object::None) => 1, // stdout
            _ => return Err(type_error("an integer is required")),
        };
        let handle = unsafe { crt::_get_osfhandle(fd) };
        if handle == -1 || handle == -2 {
            return Err(crt_error_to_py(crate::py_errno::EBADF, None));
        }
        // SAFETY: `info` is plain-old-data filled by the call on success.
        let mut info: CONSOLE_SCREEN_BUFFER_INFO = unsafe { std::mem::zeroed() };
        if unsafe { GetConsoleScreenBufferInfo(handle as _, &raw mut info) } == 0 {
            return Err(last_win32_error_to_py(None));
        }
        Ok(make_terminal_size(
            i64::from(info.srWindow.Right - info.srWindow.Left + 1),
            i64::from(info.srWindow.Bottom - info.srWindow.Top + 1),
        ))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = args;
        Ok(make_terminal_size(80, 24))
    }
}

fn os_cpu_count(_args: &[Object]) -> Result<Object, RuntimeError> {
    // `-X cpu_count=N` / `PYTHON_CPU_COUNT` (gh-109595) overrides both
    // `os.cpu_count()` and `os.process_cpu_count()`.
    if let Some(n) = crate::vm_singletons::cpu_count_override() {
        return Ok(Object::Int(n));
    }
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

// POSIX-only surface (the registrations are `#[cfg(unix)]`; CPython's `nt`
// module has no uid/gid notion at all).
#[cfg(unix)]
fn os_getuid(args: &[Object]) -> Result<Object, RuntimeError> {
    require_no_args(args, "getuid")?;
    Ok(Object::Int(i64::from(unsafe { libc::getuid() })))
}

#[cfg(unix)]
fn os_getgid(args: &[Object]) -> Result<Object, RuntimeError> {
    require_no_args(args, "getgid")?;
    Ok(Object::Int(i64::from(unsafe { libc::getgid() })))
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
        Some(other) => other.as_i64().ok_or_else(|| {
            type_error(format!(
                "{what} should be integer, not {}",
                other.type_name()
            ))
        })?,
        None => return Err(type_error(format!("{what} should be integer"))),
    };
    if value == -1 {
        return Ok(u32::MAX);
    }
    if value < 0 || value > i64::from(u32::MAX) {
        return Err(crate::error::overflow_error(format!(
            "{what} is not in range"
        )));
    }
    Ok(value as u32)
}

#[cfg(unix)]
fn os_setuid(args: &[Object]) -> Result<Object, RuntimeError> {
    let uid = id_arg(args, 0, "uid")?;
    if unsafe { libc::setuid(uid as libc::uid_t) } != 0 {
        return Err(crate::error::io_error_to_py(
            &std::io::Error::last_os_error(),
        ));
    }
    Ok(Object::None)
}

#[cfg(unix)]
fn os_setgid(args: &[Object]) -> Result<Object, RuntimeError> {
    let gid = id_arg(args, 0, "gid")?;
    if unsafe { libc::setgid(gid as libc::gid_t) } != 0 {
        return Err(crate::error::io_error_to_py(
            &std::io::Error::last_os_error(),
        ));
    }
    Ok(Object::None)
}

#[cfg(unix)]
fn os_seteuid(args: &[Object]) -> Result<Object, RuntimeError> {
    let uid = id_arg(args, 0, "uid")?;
    if unsafe { libc::seteuid(uid as libc::uid_t) } != 0 {
        return Err(crate::error::io_error_to_py(
            &std::io::Error::last_os_error(),
        ));
    }
    Ok(Object::None)
}

#[cfg(unix)]
fn os_setegid(args: &[Object]) -> Result<Object, RuntimeError> {
    let gid = id_arg(args, 0, "gid")?;
    if unsafe { libc::setegid(gid as libc::gid_t) } != 0 {
        return Err(crate::error::io_error_to_py(
            &std::io::Error::last_os_error(),
        ));
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
        return Err(crate::error::io_error_to_py(
            &std::io::Error::last_os_error(),
        ));
    }
    Ok(Object::None)
}

#[cfg(unix)]
fn os_setregid(args: &[Object]) -> Result<Object, RuntimeError> {
    let rgid = id_arg_or_keep(args, 0, "rgid")? as libc::gid_t;
    let egid = id_arg_or_keep(args, 1, "egid")? as libc::gid_t;
    if unsafe { libc::setregid(rgid, egid) } != 0 {
        return Err(crate::error::io_error_to_py(
            &std::io::Error::last_os_error(),
        ));
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
    // Windows: CPython exposes `os.umask` via the CRT's `_umask` (only the
    // `_S_IWRITE` bit is meaningful there, but the returned previous mask
    // must round-trip).
    #[cfg(windows)]
    {
        unsafe extern "C" {
            fn _umask(pmode: i32) -> i32;
        }
        let old = unsafe { _umask(mask as i32) };
        Ok(Object::Int(i64::from(old)))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = mask;
        Ok(Object::Int(0))
    }
}

fn os_symlink(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    // CPython's signature is `symlink(src, dst, target_is_directory=False, *,
    // dir_fd=None)`; `src`/`dst` are accepted positionally *or* by keyword
    // (`test_os.test_symlink_keywords`). Both ends accept `os.PathLike`
    // (`pathlib.Path`), str, or bytes.
    let src = path_arg_or_kw(args, 0, "src", kwargs, "symlink")?;
    let dst = path_arg_or_kw(args, 1, "dst", kwargs, "symlink")?;
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
        std::os::unix::fs::symlink(&src, &dst)
            .map_err(|e| crate::error::io_error_to_py_named2(&e, Some(&src), Some(&dst)))?;
        Ok(Object::None)
    }
    // Windows: file and directory links are distinct object types
    // (`CreateSymbolicLinkW` flag), selected by `target_is_directory` —
    // exactly CPython's `os_symlink_impl` under `MS_WINDOWS`.
    #[cfg(windows)]
    {
        let target_is_directory = match args.get(2).or_else(|| {
            kwargs
                .iter()
                .find(|(k, _)| k == "target_is_directory")
                .map(|(_, v)| v)
        }) {
            None | Some(Object::None) => false,
            Some(Object::Bool(b)) => *b,
            Some(Object::Int(n)) => *n != 0,
            Some(_) => return Err(type_error("symlink() target_is_directory must be bool")),
        };
        let res = if target_is_directory {
            std::os::windows::fs::symlink_dir(&src, &dst)
        } else {
            std::os::windows::fs::symlink_file(&src, &dst)
        };
        res.map_err(|e| crate::error::io_error_to_py_named2(&e, Some(&src), Some(&dst)))?;
        Ok(Object::None)
    }
    #[cfg(not(any(unix, windows)))]
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
    std::fs::hard_link(&src, &dst).map_err(|e| path_io_err2(&e, args.first(), &src, &dst))?;
    Ok(Object::None)
}

/// `os.chmod(path, mode, *, dir_fd=None, follow_symlinks=True)`. `shutil`'s
/// `copymode`/`copystat` and `_copytree` pass `follow_symlinks=`; on a symlink
/// with `follow_symlinks=False` we chmod the link via `fchmodat` where the
/// platform supports it, else fall back to the target (matching CPython's
/// best-effort `lchmod` behaviour on Linux).
/// `os.fchmod(fd, mode)` — change the permission bits of an open file
/// descriptor (`posix.fchmod`; `test_posix.test_fchmod_file`). A thin
/// wrapper over `fchmod(2)`. Unix-only, like CPython (`HAVE_FCHMOD`).
#[cfg(unix)]
fn os_fchmod(args: &[Object]) -> Result<Object, RuntimeError> {
    let fd = match args.first() {
        Some(Object::Int(n)) => *n,
        _ => return Err(type_error("fchmod() fd must be int")),
    };
    let mode = match args.get(1) {
        Some(Object::Int(m)) => *m,
        _ => return Err(type_error("fchmod() mode must be int")),
    };
    // SAFETY: plain syscall on a caller-supplied descriptor.
    let rc = unsafe { libc::fchmod(fd as libc::c_int, mode as libc::mode_t) };
    if rc != 0 {
        return Err(crate::error::io_error_to_py(
            &std::io::Error::last_os_error(),
        ));
    }
    Ok(Object::None)
}

fn os_chmod(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    reject_dir_fd(kwargs, "chmod")?;
    // CPython's `os.chmod` accepts an open file descriptor in place of a path
    // and dispatches to `fchmod(2)` (`test_posix.test_fchmod_file` calls
    // `posix.chmod(fd, mode)`) — but only where `HAVE_FCHMOD`; on Windows the
    // path converter rejects the fd form, so an int falls through to the path
    // conversion below and raises `TypeError` there, matching CPython.
    #[cfg(unix)]
    if let Some(Object::Int(_)) = args.first() {
        return os_fchmod(args);
    }
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
                return Err(path_io_err(
                    &std::io::Error::last_os_error(),
                    args.first(),
                    &p,
                ));
            }
            return Ok(Object::None);
        }
        let mut perms = std::fs::metadata(&p)
            .map_err(|e| path_io_err(&e, args.first(), &p))?
            .permissions();
        perms.set_mode(mode);
        std::fs::set_permissions(&p, perms).map_err(|e| path_io_err(&e, args.first(), &p))?;
        Ok(Object::None)
    }
    // Windows: the only chmod-able bit is FILE_ATTRIBUTE_READONLY, driven by
    // the owner-write bit — CPython's `os_chmod_impl` sets the attribute iff
    // `!(mode & _S_IWRITE)`. `follow_symlinks` is accepted-and-ignored like
    // CPython's default there (the attribute lives on the target).
    #[cfg(windows)]
    {
        let _ = follow;
        let mut perms = std::fs::metadata(&p)
            .map_err(|e| path_io_err(&e, args.first(), &p))?
            .permissions();
        // Clearing readonly is exactly the requested operation here, not an
        // oversight (the clippy lint guards accidental world-writability,
        // which has no NT analogue).
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(mode & 0o200 == 0);
        std::fs::set_permissions(&p, perms).map_err(|e| path_io_err(&e, args.first(), &p))?;
        Ok(Object::None)
    }
    #[cfg(not(any(unix, windows)))]
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
    // `times` is positional-or-keyword in CPython; `test_os.UtimeTests`
    // exercises both `os.utime(p, (a, m))` and `os.utime(p, times=(a, m))`.
    let times = match args.get(1).cloned().filter(|o| !matches!(o, Object::None)) {
        Some(t) => Some(t),
        None => kw("times"),
    };
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
            return Err(crate::error::io_error_to_py_named(
                &std::io::Error::last_os_error(),
                Some(&p),
            ));
        }
        Ok(Object::None)
    }
    // Windows has no `utimensat`: CPython's `os_utime_impl` opens the file
    // with `FILE_WRITE_ATTRIBUTES` (+ `FILE_FLAG_BACKUP_SEMANTICS` so
    // directories open too, + `OPEN_REPARSE_POINT` when not following
    // symlinks) and calls `SetFileTime`.
    #[cfg(windows)]
    {
        use crate::stdlib::nt_support::{last_win32_error_to_py, wide};
        use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::Storage::FileSystem::{
            CreateFileW, SetFileTime, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
            FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_WRITE_ATTRIBUTES,
            OPEN_EXISTING,
        };
        use windows_sys::Win32::System::SystemInformation::GetSystemTimeAsFileTime;
        // Nanoseconds-since-epoch → FILETIME (100ns ticks since 1601-01-01).
        const EPOCH_DELTA_100NS: i64 = 116_444_736_000_000_000;
        let to_filetime = |ns: i64| {
            let ticks = (ns.div_euclid(100) + EPOCH_DELTA_100NS) as u64;
            FILETIME {
                dwLowDateTime: (ticks & 0xFFFF_FFFF) as u32,
                dwHighDateTime: (ticks >> 32) as u32,
            }
        };
        let (aft, mft) = if let Some(ns_obj) = ns {
            let (a, m) = utime_pair_int(&ns_obj, "ns")?;
            (to_filetime(a), to_filetime(m))
        } else if let Some(t_obj) = times {
            let (a, m) = utime_pair_float(&t_obj, "times")?;
            (to_filetime((a * 1e9) as i64), to_filetime((m * 1e9) as i64))
        } else {
            // SAFETY: plain out-param fill.
            let mut now = FILETIME {
                dwLowDateTime: 0,
                dwHighDateTime: 0,
            };
            unsafe { GetSystemTimeAsFileTime(&raw mut now) };
            (now, now)
        };
        let mut flags = FILE_FLAG_BACKUP_SEMANTICS;
        if !dir_entry_follow(kwargs) {
            flags |= FILE_FLAG_OPEN_REPARSE_POINT;
        }
        if p.as_bytes().contains(&0) {
            return Err(value_error("embedded null character in path"));
        }
        let wpath = wide(&p);
        // SAFETY: `wpath` outlives the call; the handle is closed on every path.
        let handle = unsafe {
            CreateFileW(
                wpath.as_ptr(),
                FILE_WRITE_ATTRIBUTES,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                flags,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(last_win32_error_to_py(Some(&p)));
        }
        let ok = unsafe { SetFileTime(handle, std::ptr::null(), &raw const aft, &raw const mft) };
        let err = if ok == 0 {
            Some(last_win32_error_to_py(Some(&p)))
        } else {
            None
        };
        unsafe { CloseHandle(handle) };
        match err {
            Some(e) => Err(e),
            None => Ok(Object::None),
        }
    }
    #[cfg(not(any(unix, windows)))]
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
#[cfg(any(unix, windows))]
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
#[cfg(any(unix, windows))]
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

#[cfg(any(unix, windows))]
fn utime_pair(o: &Object, name: &str) -> Result<(Object, Object), RuntimeError> {
    // CPython requires a *tuple* of exactly two items for both `times` and `ns`
    // — a list (or any other sequence) raises TypeError, and a wrong arity too
    // (`test_os.UtimeTests.test_utime_invalid_arguments`).
    match o {
        Object::Tuple(t) if t.len() == 2 => Ok((t[0].clone(), t[1].clone())),
        _ => Err(type_error(format!(
            "utime: '{name}' must be either a tuple of two ints or None"
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
    // CPython's `os.utime` rounds the sub-second part *towards minus infinity*
    // (`_PyTime_ROUND_FLOOR`), not to nearest — `test_os.UtimeTests` relies on
    // this (it adds 0.5ns precisely so a round-to-nearest would be off by one).
    let sec = s.floor();
    let nsec = ((s - sec) * 1e9).floor() as i64;
    libc::timespec {
        tv_sec: sec as libc::time_t,
        tv_nsec: nsec.clamp(0, 999_999_999) as _,
    }
}

// ---------------------------------------------------------------------------
// RFC 0063 WS1 — the NT-only `os`/`nt` surface (posixmodule.c, MS_WINDOWS).
// ---------------------------------------------------------------------------

/// `os.getlogin()` on Windows — `GetUserNameW` (CPython's `os_getlogin_impl`
/// under `MS_WINDOWS`; the POSIX branch reads the controlling tty instead).
#[cfg(windows)]
fn os_getlogin(args: &[Object]) -> Result<Object, RuntimeError> {
    use windows_sys::Win32::System::WindowsProgramming::GetUserNameW;
    require_no_args(args, "getlogin")?;
    // UNLEN (256) + NUL, the buffer CPython sizes too.
    let mut buf = [0u16; 257];
    let mut len = buf.len() as u32;
    // SAFETY: `len` tells the API the buffer capacity; it returns the
    // written length including the terminator.
    if unsafe { GetUserNameW(buf.as_mut_ptr(), &raw mut len) } == 0 {
        return Err(crate::stdlib::nt_support::last_win32_error_to_py(None));
    }
    let n = len.saturating_sub(1) as usize;
    Ok(Object::from_str(crate::stdlib::nt_support::from_wide(
        &buf[..n],
    )))
}

/// `os.startfile(filepath, operation='open', arguments='', cwd=None,
/// show_cmd=1)` — `ShellExecuteW`, mirroring CPython's `os_startfile_impl`.
#[cfg(windows)]
fn os_startfile(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    use crate::stdlib::nt_support::wide;
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    let path = path_arg_or_kw(args, 0, "filepath", kwargs, "startfile")?;
    let kw = |name: &str| {
        kwargs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    };
    let str_arg = |o: Option<Object>, what: &str, default: &str| match o {
        None | Some(Object::None) => Ok(default.to_owned()),
        Some(Object::Str(s)) => Ok(s.to_string()),
        Some(other) => Err(type_error(format!(
            "startfile() {what} must be str, not {}",
            other.type_name()
        ))),
    };
    let operation = str_arg(
        args.get(1).cloned().or_else(|| kw("operation")),
        "operation",
        "open",
    )?;
    let arguments = str_arg(
        args.get(2).cloned().or_else(|| kw("arguments")),
        "arguments",
        "",
    )?;
    let cwd = match args.get(3).cloned().or_else(|| kw("cwd")) {
        None | Some(Object::None) => None,
        Some(o) => Some(path_to_string(&o, "startfile")?),
    };
    let show_cmd = match args.get(4).cloned().or_else(|| kw("show_cmd")) {
        None | Some(Object::None) => 1,
        Some(o) => {
            o.as_i64()
                .ok_or_else(|| type_error("startfile() show_cmd must be int"))? as i32
        }
    };
    let wpath = wide(&path);
    let wop = wide(&operation);
    let wargs = (!arguments.is_empty()).then(|| wide(&arguments));
    let wcwd = cwd.as_deref().map(wide);
    // SAFETY: every wide buffer outlives the call; NULL selects the default.
    let rc = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            wop.as_ptr(),
            wpath.as_ptr(),
            wargs.as_ref().map_or(std::ptr::null(), |w| w.as_ptr()),
            wcwd.as_ref().map_or(std::ptr::null(), |w| w.as_ptr()),
            show_cmd,
        )
    };
    // The fake-HINSTANCE result encodes failure as a value <= 32, with the
    // real Win32 error in `GetLastError` — exactly what CPython checks.
    if rc as isize <= 32 {
        return Err(crate::stdlib::nt_support::last_win32_error_to_py(Some(
            &path,
        )));
    }
    Ok(Object::None)
}

/// `os.fsync(fd)` on Windows — the CRT's `_commit` (which is
/// `FlushFileBuffers` on the fd's handle), CPython's `os_fsync_impl`.
#[cfg(windows)]
fn os_fsync(args: &[Object]) -> Result<Object, RuntimeError> {
    let fd = match args.first() {
        Some(Object::Int(i)) => *i as i32,
        Some(Object::Bool(b)) => {
            warn_bool_as_fd()?;
            i32::from(*b)
        }
        _ => return Err(type_error("fsync() arg must be int")),
    };
    let rc = unsafe { crate::stdlib::nt_support::crt::_commit(fd) };
    if rc != 0 {
        return Err(crate::stdlib::nt_support::last_crt_error_to_py(None));
    }
    Ok(Object::None)
}

/// `os.add_dll_directory(path)` — RFC 0064 WS2, CPython's
/// `Lib/os.py` + `nt._add_dll_directory`: fire the PEP 578
/// `os.add_dll_directory` audit event, register the directory with
/// the loader (`AddDllDirectory`), and hand back an
/// `_AddedDllDirectory` whose `close()` (also `__exit__`) removes it
/// again. The API itself rejects relative/nonexistent paths, which
/// surfaces as the CPython-shaped `OSError`.
#[cfg(windows)]
fn os_add_dll_directory(
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    use crate::stdlib::nt_support::wide;
    use windows_sys::Win32::System::LibraryLoader::AddDllDirectory;
    let path = path_arg_or_kw(args, 0, "path", kwargs, "add_dll_directory")?;
    crate::stdlib::sys::audit_event("os.add_dll_directory", &[Object::from_str(path.clone())])?;
    let wpath = wide(&path);
    // SAFETY: `wpath` is NUL-terminated UTF-16 and outlives the call.
    let cookie = unsafe { AddDllDirectory(wpath.as_ptr()) };
    if cookie.is_null() {
        return Err(crate::stdlib::nt_support::last_win32_error_to_py(Some(
            &path,
        )));
    }
    Ok(build_added_dll_directory(path, cookie as usize as i64))
}

/// The shared `_AddedDllDirectory` type: `close()`, context-manager
/// protocol, and CPython's repr (`<AddedDllDirectory('C:\\dir')>`,
/// `<AddedDllDirectory()>` once closed). State lives on the instance
/// (`_path`, `_cookie`); `close()` mirrors CPython in calling
/// `RemoveDllDirectory` unconditionally, so a double close raises
/// `OSError` exactly as a stale cookie does there.
#[cfg(windows)]
fn added_dll_directory_type() -> Rc<crate::types::TypeObject> {
    use crate::object::BuiltinFn;
    use crate::types::TypeObject;
    thread_local! {
        static CLS: RefCell<Option<Rc<TypeObject>>> = const { RefCell::new(None) };
    }
    fn self_dict(args: &[Object]) -> Option<Rc<RefCell<DictData>>> {
        match args.first() {
            Some(Object::Instance(i)) => Some(i.dict.clone()),
            _ => None,
        }
    }
    fn close_impl(args: &[Object]) -> Result<Object, RuntimeError> {
        use windows_sys::Win32::System::LibraryLoader::RemoveDllDirectory;
        let dict = self_dict(args)
            .ok_or_else(|| type_error("close() requires an _AddedDllDirectory instance"))?;
        let cookie = dict
            .borrow()
            .get(&DictKey(Object::from_static("_cookie")))
            .and_then(Object::as_i64)
            .unwrap_or(0);
        // SAFETY: the cookie came from `AddDllDirectory`; the API
        // validates it and fails on anything stale.
        let ok = unsafe { RemoveDllDirectory(cookie as usize as *mut std::ffi::c_void) };
        if ok == 0 {
            return Err(crate::stdlib::nt_support::last_win32_error_to_py(None));
        }
        dict.borrow_mut()
            .insert(DictKey(Object::from_static("_path")), Object::None);
        Ok(Object::None)
    }
    CLS.with(|slot| {
        if let Some(c) = slot.borrow().as_ref() {
            return c.clone();
        }
        let bt = crate::builtin_types::builtin_types();
        let mut dict = DictData::default();
        dict.insert(
            DictKey(Object::from_static("close")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "close",
                binds_instance: true,
                call: Box::new(close_impl),
                call_kw: None,
            })),
        );
        dict.insert(
            DictKey(Object::from_static("__enter__")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "__enter__",
                binds_instance: true,
                call: Box::new(|args| Ok(args.first().cloned().unwrap_or(Object::None))),
                call_kw: None,
            })),
        );
        dict.insert(
            DictKey(Object::from_static("__exit__")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "__exit__",
                binds_instance: true,
                call: Box::new(|args| {
                    close_impl(args)?;
                    Ok(Object::Bool(false))
                }),
                call_kw: None,
            })),
        );
        dict.insert(
            DictKey(Object::from_static("__repr__")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "__repr__",
                binds_instance: true,
                call: Box::new(|args| {
                    let path = self_dict(args)
                        .and_then(|d| {
                            d.borrow()
                                .get(&DictKey(Object::from_static("_path")))
                                .cloned()
                        })
                        .unwrap_or(Object::None);
                    Ok(match path {
                        Object::None => Object::from_static("<AddedDllDirectory()>"),
                        p => Object::from_str(format!("<AddedDllDirectory({})>", p.repr())),
                    })
                }),
                call_kw: None,
            })),
        );
        let cls = TypeObject::new_user("_AddedDllDirectory", vec![bt.object_.clone()], dict)
            .expect("_AddedDllDirectory type");
        *slot.borrow_mut() = Some(cls.clone());
        cls
    })
}

/// Mint one `_AddedDllDirectory` instance for [`os_add_dll_directory`].
#[cfg(windows)]
fn build_added_dll_directory(path: String, cookie: i64) -> Object {
    use crate::types::PyInstance;
    let inst = Rc::new(PyInstance::new(added_dll_directory_type()));
    {
        let mut d = inst.dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("_path")),
            Object::from_str(path),
        );
        d.insert(DictKey(Object::from_static("_cookie")), Object::Int(cookie));
    }
    Object::Instance(inst)
}

/// Resolve an NT path helper argument preserving the `str`/`bytes` flavour
/// (these mirror CPython's `path_t`-converted `nt._get*` helpers, which
/// return the same type they were given).
#[cfg(windows)]
fn nt_path_arg(args: &[Object], func: &str) -> Result<(String, bool), RuntimeError> {
    let obj = args
        .first()
        .ok_or_else(|| type_error(format!("{func}() requires a path argument")))?;
    let resolved = resolve_fspath_obj(obj, func)?;
    let want_bytes = matches!(resolved, Object::Bytes(_));
    let p = match &resolved {
        Object::Str(s) => s.to_string(),
        Object::Bytes(b) => String::from_utf8_lossy(b).into_owned(),
        _ => unreachable!("resolve_fspath_obj returns str/bytes"),
    };
    if p.as_bytes().contains(&0) {
        return Err(value_error("embedded null character"));
    }
    Ok((p, want_bytes))
}

/// Re-encode an NT path helper result in the caller's flavour.
#[cfg(windows)]
fn nt_path_result(s: String, want_bytes: bool) -> Object {
    if want_bytes {
        Object::new_bytes(s.into_bytes())
    } else {
        Object::from_str(s)
    }
}

/// `nt._getfullpathname(path)` — `GetFullPathNameW`; `ntpath.abspath`'s fast
/// path (the pure-Python fallback only runs when this name is missing).
#[cfg(windows)]
fn nt_getfullpathname(args: &[Object]) -> Result<Object, RuntimeError> {
    use crate::stdlib::nt_support::{from_wide, last_win32_error_to_py, wide};
    use windows_sys::Win32::Storage::FileSystem::GetFullPathNameW;
    let (p, want_bytes) = nt_path_arg(args, "_getfullpathname")?;
    let wpath = wide(&p);
    let mut buf = vec![0u16; 1024];
    loop {
        // SAFETY: the out-buffer is sized by `buf`; a return value larger
        // than the capacity is the needed size (retry), 0 is failure.
        let n = unsafe {
            GetFullPathNameW(
                wpath.as_ptr(),
                buf.len() as u32,
                buf.as_mut_ptr(),
                std::ptr::null_mut(),
            )
        };
        if n == 0 {
            return Err(last_win32_error_to_py(Some(&p)));
        }
        if (n as usize) <= buf.len() {
            return Ok(nt_path_result(from_wide(&buf[..n as usize]), want_bytes));
        }
        buf.resize(n as usize, 0);
    }
}

/// `nt._getfinalpathname(path)` — open the file (backup semantics so
/// directories work) and ask `GetFinalPathNameByHandleW` for the resolved
/// DOS-style name; `ntpath.realpath`'s primary resolution step.
#[cfg(windows)]
fn nt_getfinalpathname(args: &[Object]) -> Result<Object, RuntimeError> {
    use crate::stdlib::nt_support::{from_wide, last_win32_error_to_py, wide};
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, GetFinalPathNameByHandleW, FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    let (p, want_bytes) = nt_path_arg(args, "_getfinalpathname")?;
    let wpath = wide(&p);
    // SAFETY: `wpath` outlives the call; the handle is closed on every path.
    let handle = unsafe {
        CreateFileW(
            wpath.as_ptr(),
            0, // attribute access only
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(last_win32_error_to_py(Some(&p)));
    }
    let mut buf = vec![0u16; 1024];
    loop {
        // 0 = FILE_NAME_NORMALIZED | VOLUME_NAME_DOS, CPython's flags.
        let n = unsafe { GetFinalPathNameByHandleW(handle, buf.as_mut_ptr(), buf.len() as u32, 0) };
        if n == 0 {
            let e = last_win32_error_to_py(Some(&p));
            unsafe { CloseHandle(handle) };
            return Err(e);
        }
        if (n as usize) <= buf.len() {
            unsafe { CloseHandle(handle) };
            return Ok(nt_path_result(from_wide(&buf[..n as usize]), want_bytes));
        }
        buf.resize(n as usize, 0);
    }
}

/// `nt._getvolumepathname(path)` — `GetVolumePathNameW`, the mount point of
/// the volume containing `path` (`ntpath.ismount`).
#[cfg(windows)]
fn nt_getvolumepathname(args: &[Object]) -> Result<Object, RuntimeError> {
    use crate::stdlib::nt_support::{from_wide_nul, last_win32_error_to_py, wide};
    use windows_sys::Win32::Storage::FileSystem::GetVolumePathNameW;
    let (p, want_bytes) = nt_path_arg(args, "_getvolumepathname")?;
    let wpath = wide(&p);
    // The mount point is never longer than the input path; CPython sizes the
    // buffer the same way (with a MAX_PATH floor).
    let mut buf = vec![0u16; wpath.len().max(260)];
    // SAFETY: the out-buffer is sized by `buf`.
    if unsafe { GetVolumePathNameW(wpath.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) } == 0 {
        return Err(last_win32_error_to_py(Some(&p)));
    }
    Ok(nt_path_result(from_wide_nul(&buf), want_bytes))
}

/// `nt._getdiskusage(path)` — `GetDiskFreeSpaceExW`, returning the
/// `(total, free)` pair the frozen `shutil.disk_usage` expects on nt.
#[cfg(windows)]
fn nt_getdiskusage(args: &[Object]) -> Result<Object, RuntimeError> {
    use crate::stdlib::nt_support::{last_win32_error_to_py, wide};
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let (p, _) = nt_path_arg(args, "_getdiskusage")?;
    let wpath = wide(&p);
    let (mut avail, mut total, mut free) = (0u64, 0u64, 0u64);
    // SAFETY: three plain out-params.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wpath.as_ptr(),
            &raw mut avail,
            &raw mut total,
            &raw mut free,
        )
    };
    if ok == 0 {
        return Err(last_win32_error_to_py(Some(&p)));
    }
    Ok(Object::new_tuple(vec![
        Object::Int(total as i64),
        Object::Int(free as i64),
    ]))
}

/// The `(drive_end, root_end)` byte offsets of `ntpath.splitroot(p)`. All
/// decision bytes are ASCII (`\\ / : ? u n c`), so the offsets always land
/// on UTF-8 boundaries and the caller can slice either flavour with them.
/// Port of posixmodule.c's `os__path_splitroot_ex_impl`.
#[cfg(windows)]
fn nt_splitroot_indices(s: &[u8]) -> (usize, usize) {
    let is_sep = |b: u8| b == b'\\' || b == b'/';
    if s.first().copied().is_some_and(is_sep) {
        if s.get(1).copied().is_some_and(is_sep) {
            // UNC (`\\server\share`) or extended UNC (`\\?\UNC\server\share`):
            // the drive runs through the share component.
            let start = if s.len() >= 8
                && s[2] == b'?'
                && is_sep(s[3])
                && s[4].eq_ignore_ascii_case(&b'u')
                && s[5].eq_ignore_ascii_case(&b'n')
                && s[6].eq_ignore_ascii_case(&b'c')
                && is_sep(s[7])
            {
                8
            } else {
                2
            };
            let Some(index) = (start..s.len()).find(|&i| is_sep(s[i])) else {
                return (s.len(), s.len());
            };
            let Some(index2) = (index + 1..s.len()).find(|&i| is_sep(s[i])) else {
                return (s.len(), s.len());
            };
            (index2, index2 + 1)
        } else {
            // Relative to the current drive's root (`\path`).
            (0, 1)
        }
    } else if s.get(1) == Some(&b':') {
        if s.get(2).copied().is_some_and(is_sep) {
            (2, 3) // absolute drive path (`C:\path`)
        } else {
            (2, 2) // drive-relative (`C:path`)
        }
    } else {
        (0, 0)
    }
}

/// `nt._path_splitroot_ex(path)` → `(drive, root, tail)`, preserving the
/// argument's `str`/`bytes` flavour; `ntpath.splitroot`'s fast path.
#[cfg(windows)]
fn nt_path_splitroot_ex(args: &[Object]) -> Result<Object, RuntimeError> {
    let obj = args
        .first()
        .ok_or_else(|| type_error("_path_splitroot_ex() requires a path argument"))?;
    let resolved = resolve_fspath_obj(obj, "_path_splitroot_ex")?;
    match &resolved {
        Object::Str(s) => {
            let full = s.to_string();
            let (d, r) = nt_splitroot_indices(full.as_bytes());
            Ok(Object::new_tuple(vec![
                Object::from_str(full[..d].to_owned()),
                Object::from_str(full[d..r].to_owned()),
                Object::from_str(full[r..].to_owned()),
            ]))
        }
        Object::Bytes(b) => {
            let (d, r) = nt_splitroot_indices(b);
            Ok(Object::new_tuple(vec![
                Object::new_bytes(b[..d].to_vec()),
                Object::new_bytes(b[d..r].to_vec()),
                Object::new_bytes(b[r..].to_vec()),
            ]))
        }
        _ => unreachable!("resolve_fspath_obj returns str/bytes"),
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
    let mut dict = DictData::default();
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
    // `os.PathLike[bytes]` → `types.GenericAlias`, exactly CPython's
    // `__class_getitem__ = classmethod(GenericAlias)` (`test_pathlike_class_getitem`).
    dict.insert(
        DictKey(Object::from_static("__class_getitem__")),
        Object::ClassMethod(MethodWrapper::new(Object::Builtin(Rc::new(BuiltinFn {
            name: "__class_getitem__",
            binds_instance: true,
            call: Box::new(|args| {
                let origin = args.first().cloned().unwrap_or(Object::None);
                let params = args.get(1).cloned().unwrap_or(Object::None);
                Ok(crate::make_generic_alias_public(origin, params))
            }),
            call_kw: None,
        })))),
    );
    // CPython's `os.PathLike` declares `__slots__ = ()`, so it contributes no
    // instance `__dict__`. Mirror that attribute *and* the `forbids_dict`
    // bookkeeping below so a faithful subclass `class A(os.PathLike):
    // __slots__ = ()` stays dict-less (`test_pathlike_subclass_slots`).
    dict.insert(
        DictKey(Object::from_static("__slots__")),
        Object::Tuple(Rc::from(Vec::new())),
    );
    let ty = TypeObject::new_with_flags(
        Box::leak(name.to_owned().into_boxed_str()),
        vec![bt.object_.clone()],
        dict,
        TypeFlags {
            is_exception: false,
            is_builtin: true,
        },
    )
    .expect("os.PathLike");
    // `os.PathLike` carries an empty `__slots__` and so forbids an instance
    // dict; a `__slots__ = ()` subclass therefore inherits "no dict" (the
    // class-creation path only propagates `forbids_dict` from bases). We are
    // the sole owner of this freshly built `Rc` before it is memoised, so the
    // in-place mutation is sound.
    // SAFETY: no other reference to `ty` exists yet (see comment above).
    unsafe {
        (*Rc::as_ptr(&ty).cast_mut()).forbids_dict = true;
    }
    ty
}

/// Process-wide memoised `os.stat_result` type. Memoisation is load-bearing
/// for *identity*: `stat`/`lstat`/`fstat`/`DirEntry.stat()` build instances of
/// this exact type, and the module exposes the very same object as
/// `os.stat_result` / `posix.stat_result`, so `isinstance(os.stat(p),
/// os.stat_result)` holds — the CPython invariant tests (and `tarfile`,
/// `shutil`, `http.server`, …) rely on.
///
/// The layout is CPython's (`Modules/posixmodule.c` `stat_result_fields`):
/// 10 sequence slots of which the trailing three are *unnamed* — those hold
/// the integer-seconds times, while the float `st_atime`/`st_mtime`/`st_ctime`
/// are hidden named members (slots 10-12), followed by the `_ns` trio and the
/// platform extras. That split is why `tuple(st)[7]` is an int while
/// `st.st_atime` is a float, and why `n_unnamed_fields == 3`
/// (test_structseq.test_match_args_with_unnamed_fields).
fn stat_result_type() -> Rc<crate::types::TypeObject> {
    #[allow(unused_mut)]
    let mut slots: Vec<Option<&'static str>> = vec![
        Some("st_mode"),
        Some("st_ino"),
        Some("st_dev"),
        Some("st_nlink"),
        Some("st_uid"),
        Some("st_gid"),
        Some("st_size"),
        None,
        None,
        None,
        Some("st_atime"),
        Some("st_mtime"),
        Some("st_ctime"),
        Some("st_atime_ns"),
        Some("st_mtime_ns"),
        Some("st_ctime_ns"),
        Some("st_blksize"),
        Some("st_blocks"),
        Some("st_rdev"),
    ];
    #[cfg(target_os = "macos")]
    slots.extend([Some("st_flags"), Some("st_gen"), Some("st_birthtime")]);
    struct_seq_type_layout("stat_result", "os", slots, 10)
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

/// Full slot layout of a CPython `PyStructSequence` type
/// (`Objects/structseq.c`). A C struct sequence has three zones: the leading
/// `n_sequence` slots form the tuple view — some of which may be *unnamed*,
/// reachable by position only (the integer-seconds `st_?time` trio of
/// `os.stat_result`) — and every slot after them is a named-only "hidden"
/// member (`tm_zone`, `st_atime_ns`, …) reachable by attribute and via the
/// constructor's `dict` argument.
pub(crate) struct StructSeqLayout {
    pub name: &'static str,
    pub module: &'static str,
    /// Every slot in index order; `None` is an unnamed slot.
    pub slots: Vec<Option<&'static str>>,
    /// How many leading slots the tuple view exposes (`n_sequence_fields`).
    pub n_sequence: usize,
}

impl StructSeqLayout {
    fn n_fields(&self) -> usize {
        self.slots.len()
    }

    fn n_unnamed(&self) -> usize {
        self.slots.iter().filter(|s| s.is_none()).count()
    }

    /// All named members in slot order — CPython's `tp_members`. Unnamed
    /// slots are skipped, so `named()[i]` for `i < n_sequence` pulls *later*
    /// names forward exactly like `tp_members[i]` (which is how `repr(st)`
    /// pairs `st_atime=` with the integer slot 7).
    fn named(&self) -> Vec<&'static str> {
        self.slots.iter().filter_map(|s| *s).collect()
    }

    fn is_named(&self, attr: &str) -> bool {
        self.slots.contains(&Some(attr))
    }
}

thread_local! {
    /// name → (memoised type, leaked layout). Memoisation keeps type
    /// identity stable across module rebuilds so `isinstance` holds.
    static STRUCT_SEQ_REGISTRY: RefCell<
        std::collections::HashMap<
            &'static str,
            (Rc<crate::types::TypeObject>, &'static StructSeqLayout),
        >,
    > = RefCell::new(std::collections::HashMap::new());
}

/// Fetch a memoised struct-sequence type (and its layout) by name.
fn struct_seq_lookup(
    name: &str,
) -> Option<(Rc<crate::types::TypeObject>, &'static StructSeqLayout)> {
    STRUCT_SEQ_REGISTRY.with(|r| r.borrow().get(name).map(|(t, l)| (t.clone(), *l)))
}

/// Is `ty` one of the memoised struct-sequence types? `type.__setattr__`
/// consults this: CPython struct-sequence types are *heap* types, so scripts
/// can set attributes on them even though every other builtin type is
/// immutable (test_structseq.test_reference_cycle stores an instance on its
/// own type).
pub(crate) fn is_struct_seq_type(ty: &Rc<crate::types::TypeObject>) -> bool {
    STRUCT_SEQ_REGISTRY.with(|r| {
        r.borrow()
            .get(ty.name.as_str())
            .is_some_and(|(t, _)| Rc::ptr_eq(t, ty))
    })
}

/// Build (and memoise, by `name`) an all-visible, all-named struct-sequence
/// type — the common shape (`os.times_result`, `os.terminal_size`,
/// `sys.flags`, …).
pub(crate) fn struct_seq_type(
    name: &'static str,
    module: &'static str,
    fields: &'static [&'static str],
) -> Rc<crate::types::TypeObject> {
    struct_seq_type_layout(
        name,
        module,
        fields.iter().map(|f| Some(*f)).collect(),
        fields.len(),
    )
}

/// Build (and memoise, by `name`) a CPython-style `PyStructSequence` type
/// with the given full slot layout: addressable by named attribute and by
/// integer index, with `__len__` == `n_sequence`, and constructible from a
/// `n_sequence..=n_fields` element sequence plus an optional `dict` of hidden
/// named fields. Backs `os.stat_result`, `time.struct_time`, etc.
pub(crate) fn struct_seq_type_layout(
    name: &'static str,
    module: &'static str,
    slots: Vec<Option<&'static str>>,
    n_sequence: usize,
) -> Rc<crate::types::TypeObject> {
    use crate::types::{TypeFlags, TypeObject};
    STRUCT_SEQ_REGISTRY.with(|reg| {
        if let Some((c, _)) = reg.borrow().get(name) {
            return c.clone();
        }
        // Leaked so the method closures can capture a `Send + Sync` handle;
        // one allocation per struct-sequence *type*, of which there is a
        // fixed handful per process.
        let layout: &'static StructSeqLayout = Box::leak(Box::new(StructSeqLayout {
            name,
            module,
            slots,
            n_sequence,
        }));
        let bt = crate::builtin_types::builtin_types();
        let mut dict = DictData::default();
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
        // CPython's struct-sequence class metadata (test_structseq
        // test_fields / test_match_args): the three counts, plus
        // `__match_args__` — the named slots up to the first unnamed one
        // (`st_mode`..`st_size` for `stat_result`, all 9 `tm_*` for
        // `struct_time`).
        dict.insert(
            DictKey(Object::from_static("n_fields")),
            Object::Int(layout.n_fields() as i64),
        );
        dict.insert(
            DictKey(Object::from_static("n_sequence_fields")),
            Object::Int(layout.n_sequence as i64),
        );
        dict.insert(
            DictKey(Object::from_static("n_unnamed_fields")),
            Object::Int(layout.n_unnamed() as i64),
        );
        let match_args: Vec<Object> = layout.slots[..layout.n_sequence]
            .iter()
            .map_while(|s| s.map(Object::from_static))
            .collect();
        dict.insert(
            DictKey(Object::from_static("__match_args__")),
            Object::new_tuple(match_args),
        );
        struct_seq_method_kw(&mut dict, "__init__", move |args, kwargs| {
            struct_seq_init(layout, args, kwargs)
        });
        // `__reduce__` makes the struct sequence picklable as
        // `(type, (visible_tuple, hidden_dict))` — CPython's `structseq_reduce`.
        struct_seq_method(&mut dict, "__reduce__", move |args| {
            struct_seq_reduce(layout, args)
        });
        // `copy.replace()` support (CPython `structseq_replace`).
        struct_seq_method_kw(&mut dict, "__replace__", move |args, kwargs| {
            struct_seq_replace(layout, args, kwargs)
        });
        struct_seq_method(&mut dict, "__getitem__", move |args| {
            struct_seq_getitem(layout, args)
        });
        struct_seq_method(&mut dict, "__len__", move |_args| {
            Ok(Object::Int(layout.n_sequence as i64))
        });
        // Now that struct sequences subclass `tuple` (for `isinstance` parity),
        // the inherited `tuple.__iter__` would look at native tuple storage,
        // which these dict-backed instances don't have. Override `__iter__` to
        // walk the visible fields (`list(time.localtime())`, `for x in st`).
        struct_seq_method(&mut dict, "__iter__", move |args| {
            let Some(Object::Instance(inst)) = args.first() else {
                return Err(type_error("__iter__ requires a struct sequence instance"));
            };
            let values = struct_seq_values(layout, inst);
            let it = Object::new_list(values).make_iter()?;
            Ok(Object::Iter(Rc::new(RefCell::new(it))))
        });
        // CPython struct sequences expose their members as read-only member
        // descriptors and carry no instance `__dict__`, so *any* attribute
        // assignment raises `AttributeError`: named fields with the member
        // descriptor's bare "readonly attribute"
        // (test_structseq.test_copy_replace_with_invisible_fields matches
        // that exact wording), unknown names with the generic message. The
        // fields themselves are populated through `inst.dict` directly in
        // Rust (`struct_seq_init` / the `*_from_meta` builders), which
        // bypasses this guard.
        struct_seq_method(&mut dict, "__setattr__", move |args| {
            let attr = match args.get(1) {
                Some(Object::Str(s)) => s.to_string(),
                _ => String::new(),
            };
            if layout.is_named(&attr) {
                Err(crate::error::attribute_error("readonly attribute"))
            } else {
                Err(crate::error::attribute_error(format!(
                    "'{name}' object has no attribute '{attr}'"
                )))
            }
        });
        // CPython struct sequences subclass `tuple`, so `==`/`!=`/`hash()`
        // compare the visible fields by value (e.g. `os.stat(a) == os.stat(a)`
        // in `test_pathlib`, and using a `stat_result` as a dict key). Compare
        // against another struct sequence of the same type or a plain tuple.
        struct_seq_method(&mut dict, "__eq__", move |args| {
            struct_seq_richcompare(layout, args, CompareKind::Eq)
        });
        struct_seq_method(&mut dict, "__ne__", move |args| {
            struct_seq_richcompare(layout, args, CompareKind::NotEq)
        });
        // Ordering too: struct sequences order like their visible tuple
        // (`strptime('Feb 29', '%b %d') < strptime('Mar 1', '%b %d')` —
        // test_strptime's leap-year default test).
        struct_seq_method(&mut dict, "__lt__", move |args| {
            struct_seq_richcompare(layout, args, CompareKind::Lt)
        });
        struct_seq_method(&mut dict, "__le__", move |args| {
            struct_seq_richcompare(layout, args, CompareKind::LtE)
        });
        struct_seq_method(&mut dict, "__gt__", move |args| {
            struct_seq_richcompare(layout, args, CompareKind::Gt)
        });
        struct_seq_method(&mut dict, "__ge__", move |args| {
            struct_seq_richcompare(layout, args, CompareKind::GtE)
        });
        struct_seq_method(&mut dict, "__hash__", move |args| {
            struct_seq_hash(layout, args)
        });
        // CPython's `structseq_repr`: `module.name(field=repr, …)` over the
        // visible slots (e.g. `time.struct_time(tm_year=2033, …)`), *not* the
        // bare tuple repr the native `tuple` base would otherwise give now
        // that struct sequences subclass `tuple`.
        struct_seq_method(&mut dict, "__repr__", move |args| {
            struct_seq_repr(layout, args)
        });
        // CPython struct sequences subclass `tuple` (`type(os.stat(...))`'s MRO
        // is `(stat_result, tuple, object)`), so `isinstance(x, tuple)` is True
        // — `imaplib.Time2Internaldate` and lots of stdlib code branch on this.
        // The visible fields are still served by the `__getitem__`/`__len__`
        // overrides above; basing on `tuple` only affects the type's MRO.
        let cls = TypeObject::new_with_flags(
            name,
            vec![bt.tuple_.clone()],
            dict,
            TypeFlags {
                is_exception: false,
                is_builtin: true,
            },
        )
        .expect("struct sequence type");
        // Named fields as member descriptors on the *type* — CPython
        // materializes every `PyMemberDef` in `tp_dict`, so
        // `type(sys.flags).debug` resolves and pydoc documents it as
        // "debug" (test_pydoc.test_structseq_member_descriptor). Reads
        // route to the instance dict where the builders store values.
        for slot in layout.slots.iter().flatten() {
            let field: &'static str = slot;
            let key = DictKey(Object::from_static(field));
            if cls.dict.borrow().contains_key(&key) {
                continue;
            }
            let type_name: &'static str = layout.name;
            let fget = Object::Builtin(Rc::new(crate::object::BuiltinFn {
                name: field,
                binds_instance: true,
                call: Box::new(move |args| {
                    let Some(Object::Instance(inst)) = args.first() else {
                        return Err(type_error(format!(
                            "descriptor '{field}' for '{type_name}' objects doesn't apply to another object"
                        )));
                    };
                    inst.dict
                        .borrow()
                        .get(&crate::object::StrKey(field))
                        .cloned()
                        .ok_or_else(|| {
                            crate::error::attribute_error(format!(
                                "'{type_name}' object has no attribute '{field}'"
                            ))
                        })
                }),
                call_kw: None,
            }));
            let prop = Object::Property(Rc::new(crate::object::PyProperty::new(
                fget,
                Object::None,
                Object::None,
                Object::None,
            )));
            crate::descr_registry::register(
                &prop,
                crate::descr_registry::DescrKind::Member,
                cls.clone(),
                field,
                None,
            );
            cls.dict.borrow_mut().insert(key, prop);
        }
        reg.borrow_mut().insert(name, (cls.clone(), layout));
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

/// A struct-sequence method that accepts keyword arguments (`__init__`'s
/// `sequence=`/`dict=`, `__replace__`'s field names).
fn struct_seq_method_kw<F>(dict: &mut DictData, name: &'static str, body: F)
where
    F: Fn(&[Object], &[(String, Object)]) -> Result<Object, RuntimeError>
        + Send
        + Sync
        + Clone
        + 'static,
{
    let body_pos = body.clone();
    dict.insert(
        DictKey(Object::from_static(name)),
        Object::Builtin(Rc::new(crate::object::BuiltinFn {
            name,
            binds_instance: true,
            call: Box::new(move |args| body_pos(args, &[])),
            call_kw: Some(Box::new(move |args, kwargs| body(args, kwargs))),
        })),
    );
}

/// `T(sequence[, dict])` — CPython's `structseq_new_impl`. The sequence must
/// provide between `n_sequence` and `n_fields` values (positionally filling
/// hidden slots past the visible ones); the optional `dict` supplies hidden
/// *named* fields for the slots the sequence didn't reach. Any dict key that
/// duplicates a positionally-filled slot — or names no consumable slot at all
/// — is a `TypeError` (test_structseq's duplicate/unknown-field tests). Tests
/// also fabricate stat results this way to drive `posixpath.ismount`,
/// `shutil` device checks, etc.
fn struct_seq_init(
    layout: &'static StructSeqLayout,
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    let name = layout.name;
    let Some(Object::Instance(inst)) = args.first() else {
        return Err(type_error(format!(
            "{name}.__init__ requires a {name} instance"
        )));
    };
    if args.len() > 3 {
        return Err(type_error(format!(
            "{name}() takes at most 2 arguments ({} given)",
            args.len() - 1
        )));
    }
    // `PyArg_ParseTupleAndKeywords(…, "O|O!:structseq", {"sequence", "dict"})`.
    let mut seq: Option<Object> = args.get(1).cloned();
    let mut dict_arg: Option<Object> = args.get(2).cloned();
    for (k, v) in kwargs {
        let slot = match k.as_str() {
            "sequence" => &mut seq,
            "dict" => &mut dict_arg,
            other => {
                return Err(type_error(format!(
                    "'{other}' is an invalid keyword argument for {name}()"
                )));
            }
        };
        if slot.is_some() {
            return Err(type_error(format!(
                "argument for {name}() given by name ('{k}') and position"
            )));
        }
        *slot = Some(v.clone());
    }
    let Some(seq) = seq else {
        return Err(type_error(format!(
            "{name}() takes at least 1 argument (0 given)"
        )));
    };
    let dict_arg = match dict_arg {
        None => None,
        Some(Object::Dict(d)) => Some(d),
        Some(other) => {
            return Err(type_error(format!(
                "{name}() argument 2 must be dict, not {}",
                other.type_name()
            )));
        }
    };
    let values = match &seq {
        Object::Tuple(items) => items.to_vec(),
        Object::List(items) => items.borrow().clone(),
        // Everything else goes through the full VM iteration protocol so a
        // raising `__getitem__` propagates its own exception
        // (test_structseq.test_eviltuple) and strings/iterators work.
        other => {
            let ptr = crate::vm_singletons::current_interpreter_ptr()
                .ok_or_else(|| type_error("constructor requires a sequence"))?;
            // SAFETY: published by the enclosing VM frame on this thread.
            let interp = unsafe { &mut *ptr };
            let globals = Rc::new(RefCell::new(DictData::default()));
            interp.collect_iterable(other, &globals)?
        }
    };
    let (min, max) = (layout.n_sequence, layout.n_fields());
    if min == max && values.len() != min {
        return Err(type_error(format!(
            "{name}() takes a {min}-sequence ({}-sequence given)",
            values.len()
        )));
    }
    if values.len() < min {
        return Err(type_error(format!(
            "{name}() takes an at least {min}-sequence ({}-sequence given)",
            values.len()
        )));
    }
    if values.len() > max {
        return Err(type_error(format!(
            "{name}() takes an at most {max}-sequence ({}-sequence given)",
            values.len()
        )));
    }
    {
        let mut d = inst.dict.borrow_mut();
        for (i, v) in values.iter().enumerate() {
            if let Some(f) = layout.slots[i] {
                d.insert(DictKey(Object::from_static(f)), v.clone());
            }
        }
        // Hidden slots the sequence didn't reach: fill from `dict`, default
        // `None`. Only these names are consumable — CPython counts the found
        // keys and errors if the dict held anything else.
        let mut n_found = 0usize;
        for i in values.len()..max {
            let f = layout.slots[i].expect("hidden struct-seq slots are named");
            let v = dict_arg
                .as_ref()
                .and_then(|d2| d2.borrow().get(&DictKey(Object::from_static(f))).cloned());
            if v.is_some() {
                n_found += 1;
            }
            d.insert(DictKey(Object::from_static(f)), v.unwrap_or(Object::None));
        }
        if let Some(d2) = &dict_arg {
            if d2.borrow().len() > n_found {
                return Err(type_error(format!(
                    "{name}() got duplicate or unexpected field name(s)"
                )));
            }
        }
    }
    let _ = inst
        .native
        .set(Object::new_tuple(values[..layout.n_sequence].to_vec()));
    // posixmodule's `statresult_new`: a stat_result initialized from a bare
    // tuple leaves the float `st_?time` members `None`; backfill them from
    // the integer-seconds sequence slots so `os.stat_result(range(10)).st_atime`
    // is `7`, like CPython.
    if name == "stat_result" {
        let mut d = inst.dict.borrow_mut();
        for (slot, f) in [(7usize, "st_atime"), (8, "st_mtime"), (9, "st_ctime")] {
            let key = DictKey(Object::from_static(f));
            if matches!(d.get(&key), None | Some(Object::None)) {
                if let Some(v) = values.get(slot) {
                    d.insert(key, v.clone());
                }
            }
        }
    }
    Ok(Object::None)
}

/// `__replace__(**kwargs)` — CPython's `structseq_replace`, the engine behind
/// `copy.replace()`: clone the instance with the given named fields (visible
/// *or* hidden) swapped. Types with unnamed fields don't support it.
fn struct_seq_replace(
    layout: &'static StructSeqLayout,
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    let name = layout.name;
    let Some(Object::Instance(inst)) = args.first() else {
        return Err(type_error(format!(
            "{name}.__replace__ requires a {name} instance"
        )));
    };
    if args.len() > 1 {
        return Err(type_error(format!(
            "{name}.__replace__ takes no positional arguments"
        )));
    }
    if layout.n_unnamed() > 0 {
        return Err(type_error(format!(
            "__replace__() is not supported for {}.{name} because it has unnamed field(s)",
            layout.module
        )));
    }
    // No unnamed fields, so named members and slots line up one-to-one.
    let named = layout.named();
    let mut vals: Vec<Object> = {
        let d = inst.dict.borrow();
        named
            .iter()
            .map(|f| {
                d.get(&DictKey(Object::from_static(f)))
                    .cloned()
                    .unwrap_or(Object::None)
            })
            .collect()
    };
    let mut unexpected: Vec<String> = Vec::new();
    for (k, v) in kwargs {
        match named.iter().position(|f| f == k) {
            Some(i) => vals[i] = v.clone(),
            None => unexpected.push(format!("'{k}'")),
        }
    }
    if !unexpected.is_empty() {
        return Err(type_error(format!(
            "Got unexpected field name(s): ([{}])",
            unexpected.join(", ")
        )));
    }
    let (ty, _) = struct_seq_lookup(name)
        .ok_or_else(|| type_error(format!("unknown struct sequence type '{name}'")))?;
    let new_inst = crate::types::PyInstance::new(ty);
    {
        let mut d = new_inst.dict.borrow_mut();
        for (f, v) in named.iter().zip(vals.iter()) {
            d.insert(DictKey(Object::from_static(f)), v.clone());
        }
    }
    let _ = new_inst
        .native
        .set(Object::new_tuple(vals[..layout.n_sequence].to_vec()));
    let obj = Object::Instance(Rc::new(new_inst));
    // CPython allocates the copy through the GC heap, so it is tracked from
    // birth (test_structseq.test_replace_gc_tracked builds a cycle out of it).
    crate::gc_trace::track(obj.clone());
    Ok(obj)
}

fn struct_seq_getitem(
    layout: &'static StructSeqLayout,
    args: &[Object],
) -> Result<Object, RuntimeError> {
    let Some(Object::Instance(inst)) = args.first() else {
        return Err(type_error("struct sequence indexing requires an instance"));
    };
    let values = struct_seq_values(layout, inst);
    // CPython struct sequences are tuple-backed, so slicing yields a plain
    // `tuple` of the selected fields (e.g. `time.localtime()[:6]`, which
    // `tarfile`/`zipfile` use to build DOS timestamps).
    if let Some(Object::Slice(s)) = args.get(1) {
        let idxs = crate::slice_indices(values.len(), s)?;
        return Ok(Object::new_tuple(
            idxs.into_iter().map(|i| values[i].clone()).collect(),
        ));
    }
    let idx = args
        .get(1)
        .and_then(Object::as_i64)
        .ok_or_else(|| type_error("struct sequence indices must be integers"))?;
    let n = values.len() as i64;
    let i = if idx < 0 { idx + n } else { idx };
    if i < 0 || i >= n {
        return Err(crate::error::index_error("tuple index out of range"));
    }
    Ok(values[i as usize].clone())
}

/// Read the sequence (tuple-view) values of a struct-sequence instance. The
/// native tuple set at construction is authoritative — it holds the unnamed
/// slots (`stat_result`'s integer times) that the named dict can't represent.
/// Instances built before the native view existed fall back to the named
/// visible fields, with `0` in any gap.
fn struct_seq_values(
    layout: &'static StructSeqLayout,
    inst: &Rc<crate::types::PyInstance>,
) -> Vec<Object> {
    if let Some(Object::Tuple(t)) = inst.native.get() {
        return t.to_vec();
    }
    let d = inst.dict.borrow();
    layout.slots[..layout.n_sequence]
        .iter()
        .map(|slot| match slot {
            Some(f) => d
                .get(&DictKey(Object::from_static(f)))
                .cloned()
                .unwrap_or(Object::Int(0)),
            None => Object::Int(0),
        })
        .collect()
}

/// `repr()` for a struct sequence — CPython's `structseq_repr`:
/// `module.name(field=value, …)` pairing `tp_members[i]` with sequence slot
/// `i`. With unnamed slots in play the names shift forward, which is exactly
/// why CPython prints `st_atime=<int seconds>` in `repr(os.stat(...))`.
fn struct_seq_repr(
    layout: &'static StructSeqLayout,
    args: &[Object],
) -> Result<Object, RuntimeError> {
    let name = layout.name;
    let Some(Object::Instance(inst)) = args.first() else {
        return Err(type_error(format!(
            "{name}.__repr__ requires a {name} instance"
        )));
    };
    let named = layout.named();
    let values = struct_seq_values(layout, inst);
    let body = named
        .iter()
        .zip(values.iter())
        .map(|(f, v)| format!("{f}={}", v.repr()))
        .collect::<Vec<_>>()
        .join(", ");
    Ok(Object::from_str(format!(
        "{}.{name}({body})",
        layout.module
    )))
}

/// `__reduce__` for a struct sequence: `(type, (visible_tuple, hidden_dict))`.
///
/// Mirrors CPython's `structseq_reduce`. The visible tuple carries the
/// sequence slots (integer `st_*time`s for `stat_result`); the hidden dict
/// carries every named member *past* the sequence (the float times, the `_ns`
/// trio, `tm_zone`, …). On unpickling, `struct_seq_init(type, (seq, dict))`
/// restores both — every dict key names a consumable hidden slot, so the
/// duplicate-field check passes.
fn struct_seq_reduce(
    layout: &'static StructSeqLayout,
    args: &[Object],
) -> Result<Object, RuntimeError> {
    let Some(Object::Instance(inst)) = args.first() else {
        return Err(type_error("struct sequence reduce requires an instance"));
    };
    let visible = Object::new_tuple(struct_seq_values(layout, inst));
    let extra = Rc::new(RefCell::new(DictData::default()));
    {
        let d = inst.dict.borrow();
        let mut e = extra.borrow_mut();
        for slot in &layout.slots[layout.n_sequence..] {
            let f = slot.expect("hidden struct-seq slots are named");
            let v = d
                .get(&DictKey(Object::from_static(f)))
                .cloned()
                .unwrap_or(Object::None);
            e.insert(DictKey(Object::from_static(f)), v);
        }
    }
    let cls = struct_seq_lookup(layout.name)
        .map(|(t, _)| Object::Type(t))
        .ok_or_else(|| type_error("unknown struct sequence type"))?;
    Ok(Object::new_tuple(vec![
        cls,
        Object::new_tuple(vec![visible, Object::Dict(extra)]),
    ]))
}

/// `__eq__`/`__ne__` for struct sequences: compare the visible fields as a
/// tuple against another instance of the *same* struct-sequence type or a
/// plain `tuple`/`list`. Anything else yields `NotImplemented` so the other
/// operand gets a chance (matching tuple semantics).
fn struct_seq_richcompare(
    layout: &'static StructSeqLayout,
    args: &[Object],
    op: CompareKind,
) -> Result<Object, RuntimeError> {
    let Some(Object::Instance(inst)) = args.first() else {
        return Err(type_error(
            "struct sequence comparison requires an instance",
        ));
    };
    let self_tuple = Object::new_tuple(struct_seq_values(layout, inst));
    let other = match args.get(1) {
        Some(Object::Instance(other_inst)) if Rc::ptr_eq(&inst.cls(), &other_inst.cls()) => {
            Object::new_tuple(struct_seq_values(layout, other_inst))
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
    layout: &'static StructSeqLayout,
    args: &[Object],
) -> Result<Object, RuntimeError> {
    let Some(Object::Instance(inst)) = args.first() else {
        return Err(type_error("struct sequence hash requires an instance"));
    };
    let tuple = Object::new_tuple(struct_seq_values(layout, inst));
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
        for (field, value) in fields.iter().zip(values.iter()) {
            d.insert(DictKey(Object::from_static(field)), value.clone());
        }
    }
    // Struct sequences subclass `tuple`, so give the instance a native tuple
    // "view" of its visible fields. The inherited `tuple` slots
    // (`__contains__`, `__add__`, `__mul__`, `index`, `count`, …) unwrap this
    // payload, so they operate on the same values the `__getitem__`/`__len__`
    // overrides expose — without us re-implementing every sequence method.
    let _ = inst.native.set(Object::new_tuple(values));
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

/// Field names for `os.uname_result` (CPython's `posix.uname_result`).
#[cfg(unix)]
const UNAME_FIELDS: [&str; 5] = ["sysname", "nodename", "release", "version", "machine"];

/// `os.uname_result` — the 5-field struct sequence returned by `os.uname()`
/// (`platform.uname`/`mac_ver` read `.machine`, `.release`, `.sysname`).
#[cfg(unix)]
fn uname_result_type() -> Rc<crate::types::TypeObject> {
    struct_seq_type("uname_result", "os", &UNAME_FIELDS)
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

/// Resolve a path argument CPython exposes as either positional or keyword
/// (`os.open(path=...)`, `os.symlink(src=..., dst=...)`). The positional slot
/// wins; otherwise the named keyword is consulted. CPython's argument-clinic
/// signatures all accept these by name.
fn path_arg_or_kw(
    args: &[Object],
    pos: usize,
    kw_name: &str,
    kwargs: &[(String, Object)],
    func: &str,
) -> Result<String, RuntimeError> {
    if let Some(obj) = args.get(pos) {
        return path_to_string(obj, func);
    }
    if let Some((_, v)) = kwargs.iter().find(|(k, _)| k == kw_name) {
        return path_to_string(v, func);
    }
    Err(type_error(format!(
        "{func}() missing required argument: '{kw_name}' (pos {})",
        pos + 1
    )))
}

/// Fetch an integer argument from the positional slot or a keyword.
#[cfg_attr(not(unix), allow(dead_code))]
fn int_arg_or_kw(
    args: &[Object],
    pos: usize,
    kw_name: &str,
    kwargs: &[(String, Object)],
) -> Option<i64> {
    if let Some(v) = args.get(pos).and_then(Object::as_i64) {
        return Some(v);
    }
    kwargs
        .iter()
        .find(|(k, _)| k == kw_name)
        .and_then(|(_, v)| v.as_i64())
}

/// Reduce a path argument to a `str`, accepting `str`, `bytes`/`bytearray`,
/// and `os.PathLike` objects (resolved through `__fspath__`) — matching
/// CPython's `path_converter`. Shared by the `os.*` filesystem entry points.
pub(crate) fn path_to_string(obj: &Object, func: &str) -> Result<String, RuntimeError> {
    let s = match obj {
        Object::Str(s) => s.to_string(),
        // PEP 383 path converter: a surrogate-bearing `str` path is encoded
        // with the filesystem codec (UTF-8) + `surrogateescape`. A
        // non-escapable lone surrogate (e.g. U+D800) raises
        // `UnicodeEncodeError` exactly like CPython's `path_converter`
        // (test_tarfile.test_extract_unencodable). An escapable surrogate
        // (U+DC80..U+DCFF) maps back to its raw byte; the byte-faithful
        // syscall path is the deferred OsString rewrite, so a non-UTF-8 result
        // is surfaced lossily here.
        Object::WStr(cps) => {
            let bytes =
                crate::stdlib::codecs_mod::encode_codepoints(cps, "utf-8", "surrogateescape")?;
            String::from_utf8(bytes)
                .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
        }
        Object::Bytes(b) => String::from_utf8_lossy(b).into_owned(),
        Object::ByteArray(b) => String::from_utf8_lossy(&b.borrow()).into_owned(),
        // A `str`/`bytes` *subclass* instance is itself the path: CPython's
        // `path_converter` treats `PyUnicode_Check`/`PyBytes_Check` subclasses
        // as the string *before* consulting `__fspath__` (`test_oserror_filename`
        // passes a `class Str(str)` instance).
        Object::Instance(_)
            if matches!(obj.native_value(), Some(Object::Str(_) | Object::Bytes(_))) =>
        {
            match obj.native_value() {
                Some(Object::Str(s)) => s.to_string(),
                Some(Object::Bytes(b)) => String::from_utf8_lossy(&b).into_owned(),
                _ => unreachable!("guarded by the match arm above"),
            }
        }
        Object::Instance(_) => {
            let ptr = crate::vm_singletons::current_interpreter_ptr().ok_or_else(|| {
                type_error(format!(
                    "{func}: path should be string, bytes or os.PathLike, not {}",
                    obj.type_name_owned()
                ))
            })?;
            // SAFETY: published by the enclosing VM frame on this thread.
            let interp = unsafe { &mut *ptr };
            // A missing `__fspath__` *or* one explicitly set to `None`
            // (`class Foo: __fspath__ = None`) means "not path-like": CPython's
            // `path_converter` raises the canonical TypeError rather than trying
            // to call `None` (`test_os.test_fspath_set_to_None`).
            let fspath = match interp.load_attr_public(obj, "__fspath__") {
                Ok(Object::None) | Err(_) => {
                    return Err(type_error(format!(
                        "{func}: path should be string, bytes or os.PathLike, not {}",
                        obj.type_name_owned()
                    )))
                }
                Ok(m) => m,
            };
            match interp.call_object(fspath, &[], &[])? {
                Object::Str(s) => s.to_string(),
                Object::Bytes(b) => String::from_utf8_lossy(&b).into_owned(),
                // A surrogate-bearing str (pathlib's surrogateescape'd
                // names) is still a str result: recurse into the WStr
                // arm's PEP 383 encoding (test_pathlib's
                // `P(base + '\udfff').exists()` probes).
                w @ Object::WStr(_) => path_to_string(&w, func)?,
                other => {
                    return Err(type_error(format!(
                        "expected {}.__fspath__() to return str or bytes, not {}",
                        obj.type_name_owned(),
                        other.type_name_owned()
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

    // `os.makedirs`' split must be `ntpath.split` on Windows: sysconfig
    // normpaths every install-scheme path to backslashes, so venv hands
    // `makedirs` `{env}\Lib\site-packages` — a `/`-only split sees one
    // giant leaf, skips parent creation, and the leaf `mkdir` dies with
    // ERROR_PATH_NOT_FOUND (the RFC 0063 dist-check venv leg).
    #[cfg(windows)]
    #[test]
    fn nt_split_mirrors_ntpath() {
        assert_eq!(
            nt_split(r"C:\venv\Lib\site-packages"),
            (r"C:\venv\Lib", "site-packages")
        );
        assert_eq!(nt_split(r"C:\venv/Lib"), (r"C:\venv", "Lib"));
        assert_eq!(nt_split(r"C:\x\"), (r"C:\x", ""));
        assert_eq!(nt_split(r"C:\"), (r"C:\", ""));
        assert_eq!(nt_split("C:x"), ("C:", "x"));
        assert_eq!(nt_split("rel"), ("", "rel"));
        assert_eq!(nt_split(r"a\b"), ("a", "b"));
        assert_eq!(
            nt_split(r"\\server\share\dir\f"),
            (r"\\server\share\dir", "f")
        );
        assert_eq!(nt_split(r"\\server\share"), (r"\\server\share", ""));
        assert_eq!(nt_split(r"\\?\C:\x\y"), (r"\\?\C:\x", "y"));
    }

    #[cfg(windows)]
    #[test]
    fn nt_splitdrive_mirrors_ntpath() {
        assert_eq!(nt_splitdrive(r"C:\x"), ("C:", r"\x"));
        assert_eq!(
            nt_splitdrive(r"\\server\share\x"),
            (r"\\server\share", r"\x")
        );
        assert_eq!(nt_splitdrive(r"\\?\C:\x"), (r"\\?\C:", r"\x"));
        assert_eq!(nt_splitdrive(r"\x\y"), ("", r"\x\y"));
        assert_eq!(nt_splitdrive("rel"), ("", "rel"));
    }
}
