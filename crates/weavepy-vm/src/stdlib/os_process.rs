//! POSIX process & low-level fd primitives for the `os` module (RFC 0040
//! WS1). These are the foundation the faithful `subprocess` /
//! `multiprocessing` / `signal` stack rides on: `fork`/`exec*`,
//! `posix_spawn`, `wait*` + the `W*` status macros, process-group /
//! session control, `closerange`, `register_at_fork`, and a few small
//! `os` surface gaps (`environb`, `device_encoding`) that `test_os`
//! probes.
//!
//! The POSIX-only surface is gated to `unix` so those names don't exist in
//! the module dict elsewhere (RFC 0063: CPython-on-Windows exports none of
//! them). Windows gets its own arms for the portable subset plus the CRT
//! `spawnv` family. Tracks CPython 3.13's `posixmodule.c`.

#![allow(clippy::unnecessary_wraps)]

use super::os::builtin;
#[cfg(unix)]
use super::os::builtin_kw;
#[cfg(any(unix, windows))]
use crate::error::value_error;
use crate::error::{type_error, RuntimeError};
use crate::object::{DictData, DictKey, Object};
// Only the unix/windows-gated helpers (`env_mapping_dict`,
// `environb_snapshot`) need these; an unconditional import trips
// `-D warnings` on other targets.
#[cfg(any(unix, windows))]
use crate::sync::{Rc, RefCell};
#[cfg(unix)]
use parking_lot::Mutex;

#[cfg(unix)]
use std::ffi::CString;

/// Install every process/fd primitive into the `os` module dict.
pub(super) fn register(d: &mut DictData) {
    macro_rules! reg {
        ($name:literal, $f:expr) => {
            d.insert(DictKey(Object::from_static($name)), builtin($name, $f));
        };
    }
    #[cfg_attr(not(unix), allow(unused_macros))]
    macro_rules! reg_kw {
        ($name:literal, $f:expr) => {
            d.insert(DictKey(Object::from_static($name)), builtin_kw($name, $f));
        };
    }
    #[cfg_attr(not(any(unix, windows)), allow(unused_macros))]
    macro_rules! con {
        ($name:literal, $v:expr) => {
            d.insert(DictKey(Object::from_static($name)), Object::Int($v));
        };
    }

    // RFC 0063 WS1: everything POSIX-only is gated so the names simply do
    // not exist in the `nt` module dict — CPython-on-Windows exports none of
    // them, and code in the wild feature-detects with `hasattr(os, 'fork')`
    // etc., so a raising stub would be worse than absence.
    #[cfg(unix)]
    {
        // --- process creation / replacement ---
        reg!("fork", os_fork);
        reg!("execv", os_execv);
        reg!("execve", os_execve);
        reg!("execvp", os_execvp);
        reg!("execvpe", os_execvpe);
        reg_kw!("posix_spawn", os_posix_spawn);
        reg_kw!("posix_spawnp", os_posix_spawnp);
        reg_kw!("register_at_fork", register_at_fork_kw);

        // --- waiting ---
        reg!("wait", os_wait);
        reg!("wait3", os_wait3);
        reg!("wait4", os_wait4);

        // --- W* status macros ---
        reg!("WIFEXITED", w_ifexited);
        reg!("WEXITSTATUS", w_exitstatus);
        reg!("WIFSIGNALED", w_ifsignaled);
        reg!("WTERMSIG", w_termsig);
        reg!("WIFSTOPPED", w_ifstopped);
        reg!("WSTOPSIG", w_stopsig);
        reg!("WIFCONTINUED", w_ifcontinued);
        reg!("WCOREDUMP", w_coredump);

        // --- process groups / sessions ---
        reg!("setsid", os_setsid);
        reg!("getsid", os_getsid);
        reg!("setpgid", os_setpgid);
        reg!("getpgid", os_getpgid);
        reg!("getpgrp", os_getpgrp);
        reg!("setpgrp", os_setpgrp);
        reg!("tcgetpgrp", os_tcgetpgrp);
        reg!("tcsetpgrp", os_tcsetpgrp);
        reg!("killpg", os_killpg);

        // --- fd helpers / ids ---
        reg!("pipe2", os_pipe2);
        reg!("setuid", os_setuid);
        reg!("setgid", os_setgid);
        reg!("setegid", os_setegid);
        reg!("seteuid", os_seteuid);
        reg!("setgroups", os_setgroups);

        // `sched_yield` is `HAVE_SCHED_H` surface — absent on Windows.
        reg!("sched_yield", os_sched_yield);

        // --- W* / wait option constants ---
        con!("WUNTRACED", i64::from(WUNTRACED));
        con!("WCONTINUED", i64::from(WCONTINUED));

        // --- posix_spawn file-action selectors (CPython's own enum, 0/1/2) ---
        con!("POSIX_SPAWN_OPEN", 0);
        con!("POSIX_SPAWN_CLOSE", 1);
        con!("POSIX_SPAWN_DUP2", 2);

        // --- sysexits-style exit codes (`<sysexits.h>`, absent on NT) ---
        con!("EX_OK", 0);
        con!("EX_USAGE", 64);
        con!("EX_DATAERR", 65);
        con!("EX_NOINPUT", 66);
        con!("EX_NOUSER", 67);
        con!("EX_NOHOST", 68);
        con!("EX_UNAVAILABLE", 69);
        con!("EX_SOFTWARE", 70);
        con!("EX_OSERR", 71);
        con!("EX_OSFILE", 72);
        con!("EX_CANTCREAT", 73);
        con!("EX_IOERR", 74);
        con!("EX_TEMPFAIL", 75);
        con!("EX_PROTOCOL", 76);
        con!("EX_NOPERM", 77);
        con!("EX_CONFIG", 78);
    }

    // --- portable surface (real Windows arms below) ---
    reg!("_exit", os_exit_now);
    reg!("abort", os_abort);
    reg!("closerange", os_closerange);
    reg!("getppid", os_getppid);
    reg!("device_encoding", os_device_encoding);

    // --- affinity / scheduling ---
    // CPU affinity is a Linux-only surface; CPython doesn't expose
    // `sched_{get,set}affinity` on macOS/BSD, so neither do we (a guarded
    // `hasattr` then drives the fallback — `test_posix.test_sched_getaffinity`
    // skips rather than erroring).
    #[cfg(target_os = "linux")]
    {
        reg!("sched_getaffinity", os_sched_getaffinity);
        reg!("sched_setaffinity", os_sched_setaffinity);
    }

    #[cfg(target_os = "linux")]
    {
        con!("WEXITED", i64::from(libc::WEXITED));
        con!("WSTOPPED", i64::from(libc::WSTOPPED));
        con!("WNOWAIT", i64::from(libc::WNOWAIT));
        con!("P_ALL", i64::from(libc::P_ALL));
        con!("P_PID", i64::from(libc::P_PID));
        con!("P_PGID", i64::from(libc::P_PGID));
    }

    // --- dynamic-loader (`dlopen(3)`) mode flags ---
    // CPython's `posix`/`os` expose the `RTLD_*` bits used by `ctypes` and by
    // `sys.setdlopenflags`. Values are platform-specific, so source them from
    // `libc` rather than hardcoding (`test_posix.test_rtld_constants` asserts
    // the four canonical names exist).
    #[cfg(unix)]
    {
        con!("RTLD_LAZY", i64::from(libc::RTLD_LAZY));
        con!("RTLD_NOW", i64::from(libc::RTLD_NOW));
        con!("RTLD_GLOBAL", i64::from(libc::RTLD_GLOBAL));
        con!("RTLD_LOCAL", i64::from(libc::RTLD_LOCAL));
        con!("RTLD_NODELETE", i64::from(libc::RTLD_NODELETE));
        con!("RTLD_NOLOAD", i64::from(libc::RTLD_NOLOAD));
        #[cfg(target_os = "linux")]
        con!("RTLD_DEEPBIND", i64::from(libc::RTLD_DEEPBIND));
    }

    // --- Windows-only: the CRT spawn family (posixmodule.c `os_spawnv_impl`/
    // `os_spawnve_impl` under `HAVE_WSPAWNV`) plus its `P_*` mode constants
    // (`process.h` values). `os.waitpid` accepts the P_NOWAIT handle.
    #[cfg(windows)]
    {
        reg!("spawnv", os_spawnv);
        reg!("spawnve", os_spawnve);
        con!("P_WAIT", 0);
        con!("P_NOWAIT", 1);
        con!("P_OVERLAY", 2);
        con!("P_NOWAITO", 3);
        con!("P_DETACH", 4);
    }

    // `environb` — a bytes-keyed/-valued view of the environment. CPython
    // builds it lazily from the raw `environ` block; we snapshot at import
    // (writes go through `os.environ`; `environb` is read-mostly in tests).
    // CPython only defines it where `supports_bytes_environ` is True, i.e.
    // not on Windows — callers there probe with `getattr`/`hasattr`.
    #[cfg(unix)]
    d.insert(
        DictKey(Object::from_static("environb")),
        environb_snapshot(),
    );
}

#[cfg(unix)]
const WUNTRACED: libc::c_int = libc::WUNTRACED;
#[cfg(target_os = "linux")]
const WCONTINUED: libc::c_int = libc::WCONTINUED;
#[cfg(all(unix, not(target_os = "linux")))]
const WCONTINUED: libc::c_int = 0x10;

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn obj_to_cstring(o: &Object, what: &str) -> Result<CString, RuntimeError> {
    let bytes: Vec<u8> = match o {
        Object::Str(s) => s.as_bytes().to_vec(),
        // PEP 383: lone-surrogate `str` arguments fs-encode with
        // `surrogateescape` (CPython's `PyUnicode_FSConverter`).
        Object::WStr(cps) => {
            crate::stdlib::codecs_mod::encode_codepoints(cps, "utf-8", "surrogateescape")?
        }
        Object::Bytes(b) => b.to_vec(),
        Object::ByteArray(b) => b.borrow().clone(),
        // `os.PathLike` — CPython's exec/spawn path arguments go through
        // `PyUnicode_FSConverter`, which honours `__fspath__` (e.g. the
        // `FakePath` wrappers in `test_os`). Resolve it the same way.
        Object::Instance(_) => crate::stdlib::os::path_to_string(o, what)?.into_bytes(),
        _ => return Err(type_error(format!("{what}: expected str or bytes"))),
    };
    CString::new(bytes).map_err(|_| value_error(format!("{what}: embedded null byte")))
}

#[cfg(any(unix, windows))]
fn obj_to_int(o: &Object, what: &str) -> Result<i64, RuntimeError> {
    // `as_i64` also unwraps int subclasses (e.g. `signal.Signals` enum
    // members), matching CPython's `__index__` coercion for these args.
    o.as_i64()
        .ok_or_else(|| type_error(format!("{what}: an integer is required")))
}

/// Build a NULL-terminated `*const c_char` array from an iterable of
/// str/bytes. Returns the `CString`s (which own the storage) plus the
/// pointer vector — the caller must keep both alive across the syscall.
#[cfg(unix)]
fn build_argv(
    seq: &Object,
    what: &str,
) -> Result<(Vec<CString>, Vec<*const libc::c_char>), RuntimeError> {
    let items = crate::stdlib::os::sequence_items(seq)
        .ok_or_else(|| type_error(format!("{what}: argv must be a sequence of str/bytes")))?;
    if items.is_empty() {
        return Err(value_error(format!("{what}: argv must not be empty")));
    }
    let mut owned: Vec<CString> = Vec::with_capacity(items.len());
    for it in &items {
        owned.push(obj_to_cstring(it, what)?);
    }
    let mut ptrs: Vec<*const libc::c_char> = owned.iter().map(|c| c.as_ptr()).collect();
    ptrs.push(std::ptr::null());
    Ok((owned, ptrs))
}

/// Build a NULL-terminated envp from a mapping (`dict`) or a sequence of
/// `b"K=V"` items. `subprocess` passes a `dict`; `execve` accepts either.
#[cfg(unix)]
fn build_envp(
    env: &Object,
    what: &str,
) -> Result<(Vec<CString>, Vec<*const libc::c_char>), RuntimeError> {
    let mut owned: Vec<CString> = Vec::new();
    // The environment may arrive as a plain `dict`, as one of `os.environ`/
    // `os.environb` (the `_Environ` mappings whose canonical bytes->bytes
    // store is the `_data` attribute, RFC 0040 WS1), or as a pre-formatted
    // sequence of `b"KEY=VALUE"` items. Resolve the mapping form to its
    // backing dict so the `KEY=VALUE` encoding is shared.
    let mapping = env_mapping_dict(env);
    match mapping {
        Some(d) => {
            for (k, v) in d.borrow().iter() {
                let mut kv: Vec<u8> = bytes_of(&k.0).ok_or_else(|| {
                    type_error(format!("{what}: environment keys must be str/bytes"))
                })?;
                // CPython rejects an `=` in an environment variable *name* with
                // `ValueError` *before* calling `execve`. Without this guard a
                // name like `b"FRUIT=ORANGE"` is accepted by `execve(2)`, which
                // then replaces the (test-runner) process image — a silent,
                // un-gradeable crash rather than the expected exception.
                if kv.contains(&b'=') {
                    return Err(value_error("illegal environment variable name"));
                }
                kv.push(b'=');
                kv.extend_from_slice(&bytes_of(v).ok_or_else(|| {
                    type_error(format!("{what}: environment values must be str/bytes"))
                })?);
                owned.push(CString::new(kv).map_err(|_| value_error("embedded null byte"))?);
            }
        }
        None => {
            // A generic mapping (anything exposing `keys()`/`values()`):
            // CPython's `parse_envlist` calls `PyMapping_Keys`/`PyMapping_Values`
            // and zips them, fs-encoding each via `PyUnicode_FSConverter` (so
            // `os.PathLike` keys/values are honoured). We snapshot both lists up
            // front, so a `__fspath__` that mutates the original mapping mid-parse
            // can't corrupt the walk (`test_os.test_execve_env_concurrent_mutation*`).
            if let Some((keys, values)) = mapping_keys_values(env)? {
                for (k, v) in keys.iter().zip(values.iter()) {
                    let mut kv = obj_to_env_bytes(k, what)?;
                    if kv.contains(&b'=') {
                        return Err(value_error("illegal environment variable name"));
                    }
                    kv.push(b'=');
                    kv.extend_from_slice(&obj_to_env_bytes(v, what)?);
                    owned.push(CString::new(kv).map_err(|_| value_error("embedded null byte"))?);
                }
            } else {
                let items = crate::stdlib::os::sequence_items(env)
                    .ok_or_else(|| type_error(format!("{what}: env must be a dict or sequence")))?;
                for it in &items {
                    owned.push(obj_to_cstring(it, what)?);
                }
            }
        }
    }
    let mut ptrs: Vec<*const libc::c_char> = owned.iter().map(|c| c.as_ptr()).collect();
    ptrs.push(std::ptr::null());
    Ok((owned, ptrs))
}

/// Materialise a generic environment *mapping* into snapshotted key/value
/// lists by calling its `keys()` and `values()` (CPython's
/// `PyMapping_Keys`/`PyMapping_Values`). Returns `None` for a non-mapping (no
/// `keys()`/`values()`), so the caller can fall back to the sequence form.
#[cfg(unix)]
fn mapping_keys_values(env: &Object) -> Result<Option<(Vec<Object>, Vec<Object>)>, RuntimeError> {
    // Plain dicts and `_Environ` are resolved earlier; only instances reach
    // here as candidate mappings.
    if !matches!(env, Object::Instance(_)) {
        return Ok(None);
    }
    let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() else {
        return Ok(None);
    };
    // SAFETY: published by the live VM driving this call; GIL-exclusive.
    let interp = unsafe { &mut *ptr };
    let (Ok(keys_m), Ok(vals_m)) = (
        interp.load_attr_public(env, "keys"),
        interp.load_attr_public(env, "values"),
    ) else {
        return Ok(None);
    };
    let keys = interp.call_object(keys_m, &[], &[])?;
    let vals = interp.call_object(vals_m, &[], &[])?;
    Ok(Some((iterate_to_vec(&keys)?, iterate_to_vec(&vals)?)))
}

/// Eagerly drain a Python iterable into a `Vec<Object>` snapshot.
#[cfg(unix)]
fn iterate_to_vec(obj: &Object) -> Result<Vec<Object>, RuntimeError> {
    let mut it = obj
        .make_iter()
        .map_err(|_| type_error(format!("{} object is not iterable", obj.type_name())))?;
    let mut out = Vec::new();
    while let Some(v) = it.next_value() {
        out.push(v);
    }
    Ok(out)
}

/// Fs-encode an environment key/value to bytes: `str`/`bytes`/`bytearray`
/// verbatim, else honour `os.PathLike` via `__fspath__` (CPython's
/// `PyUnicode_FSConverter`).
#[cfg(unix)]
fn obj_to_env_bytes(o: &Object, what: &str) -> Result<Vec<u8>, RuntimeError> {
    if let Some(b) = bytes_of(o) {
        return Ok(b);
    }
    Ok(crate::stdlib::os::path_to_string(o, what)?.into_bytes())
}

/// Resolve an environment argument to its backing key/value `dict`.
///
/// Accepts a plain `dict` and the `_Environ` mappings (`os.environ` /
/// `os.environb`), whose canonical bytes-keyed store lives in the `_data`
/// instance attribute. Returns `None` for anything else (e.g. a sequence of
/// `KEY=VALUE` strings), letting the caller fall back to the sequence path.
#[cfg(any(unix, windows))]
fn env_mapping_dict(env: &Object) -> Option<Rc<RefCell<DictData>>> {
    match env {
        Object::Dict(d) => Some(d.clone()),
        Object::Instance(inst) => {
            match inst
                .dict
                .borrow()
                .get(&DictKey(Object::from_static("_data")))
                .cloned()
            {
                Some(Object::Dict(d)) => Some(d),
                _ => None,
            }
        }
        _ => None,
    }
}

#[cfg(unix)]
fn bytes_of(o: &Object) -> Option<Vec<u8>> {
    match o {
        Object::Str(s) => Some(s.as_bytes().to_vec()),
        // PEP 383: a lone-surrogate `str` (the regrtest harness plants one
        // in `os.environ` via PYTHONREGRTEST_UNICODE_GUARD) fs-encodes with
        // `surrogateescape`, like CPython's `PyUnicode_FSConverter`
        // (test_posix TestPosixSpawn.test_specify_environment spawns with
        // `{**os.environ, ...}`).
        Object::WStr(cps) => {
            crate::stdlib::codecs_mod::encode_codepoints(cps, "utf-8", "surrogateescape").ok()
        }
        Object::Bytes(b) => Some(b.to_vec()),
        Object::ByteArray(b) => Some(b.borrow().clone()),
        _ => None,
    }
}

#[cfg(unix)]
fn last_os_err() -> RuntimeError {
    crate::error::io_error_to_py(&std::io::Error::last_os_error())
}

// ---------------------------------------------------------------------------
// fork / exec / _exit
// ---------------------------------------------------------------------------

/// Whether the *currently executing* interpreter's PEP 684 feature
/// flags include `bit` (`Py_RTFLAGS_*`). Outside any published
/// interpreter frame everything is allowed (the main interpreter's
/// defaults carry all the process-control bits).
fn current_interp_allows(bit: u32) -> bool {
    match crate::vm_singletons::current_interpreter_ptr() {
        // SAFETY: published by the enclosing VM dispatch frame on this
        // thread; the GIL keeps the access exclusive.
        Some(p) => (unsafe { (*p).interp_settings().0 } & bit) != 0,
        None => true,
    }
}

#[cfg(unix)]
fn os_fork(_args: &[Object]) -> Result<Object, RuntimeError> {
    // CPython refuses to fork once the interpreter is tearing down (a
    // `__del__`/`atexit` that forks): `os.fork()` raises
    // `RuntimeError: can't fork at interpreter shutdown`
    // (`test_os.test_fork_at_finalization`, `test_subprocess.test_preexec_at_exit`).
    if crate::vm_singletons::is_finalizing() {
        return Err(crate::error::runtime_error(
            "can't fork at interpreter shutdown",
        ));
    }
    // PEP 684 `Py_RTFLAGS_FORK`: an isolated sub-interpreter created
    // without `allow_fork` refuses (CPython `os_fork_impl`'s
    // `interp->feature_flags` check — test__interpreters
    // RunStringTests.test_fork expects RuntimeError).
    if !current_interp_allows(1 << 15) {
        return Err(crate::error::runtime_error(
            "fork not supported for isolated subinterpreters",
        ));
    }
    // CPython 3.12+ (`Modules/posixmodule.c: warn_about_fork_with_threads`):
    // forking a multi-threaded process is hazardous, so `os.fork()` issues a
    // `DeprecationWarning`. We must sample the parent's thread state *now*
    // (only the calling thread survives `fork(2)`, so the count is gone in
    // the child) but emit the warning *after* the fork, in the parent branch
    // only — exactly like `os_fork_impl`, whose `warn_about_fork_with_threads`
    // runs after `PyOS_AfterFork_Parent`. The child therefore inherits the
    // pre-fork `warnings` state (an empty `catch_warnings(record=True)` list),
    // which is what `test_fork_warns_when_non_python_thread_exists` asserts.
    let multithreaded = process_is_multithreaded();
    run_atfork(AtForkPhase::Before);
    // `PyOS_BeforeFork` (after the Python before-handlers): take the
    // global import lock, so a subthread mid-import — which published a
    // partial module to `sys.modules` under this lock — finishes before
    // the address space is snapshotted. The child then never observes
    // partially initialized modules (test_fork1
    // test_threaded_import_lock_fork).
    crate::stdlib::imp_mod::import_lock_acquire();
    // SAFETY: `fork(2)`. In the child only this thread survives; we run the
    // registered after-in-child handlers (CPython's `PyOS_AfterFork_Child`).
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        let _ = crate::stdlib::imp_mod::import_lock_release();
        run_atfork(AtForkPhase::Parent);
        return Err(last_os_err());
    }
    if pid == 0 {
        // C-level after-fork (before the Python `_after_fork` handlers).
        // FIRST rebuild the GIL: only this thread survived, but the GIL's
        // `parking_lot` lock/condvars were copied with the vanished peers'
        // parked state still recorded. Any later GIL hand-off (the eval
        // loop's periodic checkpoint, `allow_threads` around blocking I/O,
        // or the Python `_after_fork` handlers below) would walk
        // `parking_lot`'s unpark path into a now-gone thread and wedge the
        // child — CPython's `PyOS_AfterFork_Child` does the same
        // `_PyEval_ReInitThreads` reset before anything else runs
        // (`test_threading.test_reinit_tls_after_fork`).
        crate::gil::reinit_after_fork_in_child();
        // The process-global cycle collector's `parking_lot`-backed locks are
        // not fork-safe either: a peer thread that dropped the last `Arc` to a
        // tracked object off-GIL could have vanished mid-`borrow`, leaving the
        // collector's lock held in the child. Rebuild them (preserving the
        // tracked set) before any allocation — `threading._after_fork` builds
        // `set(_enumerate())` immediately, which would otherwise wedge.
        // SAFETY: sole surviving post-fork thread.
        unsafe {
            crate::gc_trace::reinit_after_fork_in_child();
        }
        // Only this thread survived the fork, so drop the inherited
        // `JoinHandle`s for the parent's now-vanished threads. Otherwise
        // the child's shutdown join would `pthread_join` a dead thread and
        // abort with ESRCH (the multiprocessing fork start method spawns
        // workers via `os.fork()` while a queue-feeder thread is live).
        let cur = crate::vm_singletons::current_worker_thread_id();
        crate::thread_registry::registry().reset_after_fork_in_child(cur);
        // Every other OS thread vanished with the fork, so the infra-thread
        // baseline shrinks to this lone survivor. Re-sample it so a *nested*
        // fork in the child judges "multi-threaded" against the right floor.
        capture_thread_baseline();
        // gh-110395: a `kqueue` control descriptor is not inherited across
        // `fork(2)`. Close every live kqueue object in the child (before the
        // Python `after_in_child` handlers run) so it observes `kq.closed`,
        // exactly like CPython's `PyOS_AfterFork_Child` (`test_kqueue.test_fork`).
        crate::stdlib::select_mod::close_kqueues_after_fork_in_child();
        // CPython's `_PyThread_AfterFork`: the forking thread is the child's
        // new main thread, and every other thread vanished — re-seed the main
        // ident and mark the orphaned thread handles done so `Thread.is_alive()`
        // / `threading._after_fork` see them dead. Runs before the Python
        // `after_in_child` handlers (`threading._after_fork`) below.
        crate::stdlib::thread_real::after_fork_in_child();
        // `_PyThreadState_DeleteExcept`: purge the vanished peers'
        // thread-state entries so `sys._current_frames()` reports exactly
        // one frame in the child (test_threading
        // test_clear_threads_states_after_fork).
        crate::stdlib::faulthandler_mod::reinit_threads_after_fork_in_child(cur);
        // `_PyImport_ReInitLock`: drop the fork-time acquisition and hand
        // any remaining user-held levels to this (sole surviving) thread.
        crate::stdlib::imp_mod::import_lock_reinit_in_child();
        run_atfork(AtForkPhase::Child);
    } else {
        // `PyOS_AfterFork_Parent`: release the fork-time import-lock level.
        let _ = crate::stdlib::imp_mod::import_lock_release();
        run_atfork(AtForkPhase::Parent);
        if multithreaded {
            if let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() {
                // SAFETY: published by the active builtin call on this thread;
                // the interpreter outlives this synchronous re-entrant call.
                let interp = unsafe { &mut *ptr };
                // SAFETY: `getpid(2)` is always safe.
                let pid_self = unsafe { libc::getpid() };
                interp.warn_deprecation_from_builtin(format!(
                    "This process (pid={pid_self}) is multi-threaded, \
                     use of fork() may lead to deadlocks in the child."
                ))?;
            }
        }
    }
    Ok(Object::Int(i64::from(pid)))
}

/// Baseline count of WeavePy *infrastructure* OS threads — the parked
/// process-initial thread, the `weavepy-main` VM thread, and any runtime
/// threads spun up before user code runs. Captured once at interpreter
/// startup (`capture_thread_baseline`) so a later `os.fork()` can tell
/// whether the process has since acquired *additional* threads — Python
/// `threading` workers *or* raw non-Python `pthread`s created by C/Rust
/// extensions — and therefore must emit the multi-threaded-fork
/// `DeprecationWarning`. `0` until captured (treated as "unknown").
static THREAD_BASELINE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static THREAD_BASELINE_SET: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Count the live OS threads in this process, or `None` when the platform
/// query is unavailable. macOS uses the mach `task_threads` task port
/// introspection (CPython's `warn_about_fork_with_threads` does the same);
/// Linux counts the kernel thread directories under `/proc/self/task`.
#[cfg(target_os = "macos")]
pub fn count_os_threads() -> Option<usize> {
    use std::os::raw::c_uint;
    // `mach_port_t` / `thread_act_t` are `c_uint`; `kern_return_t` is `i32`.
    // `mach_task_self_` is the global the `mach_task_self()` macro expands to.
    extern "C" {
        static mach_task_self_: c_uint;
        fn task_threads(
            target_task: c_uint,
            act_list: *mut *mut c_uint,
            act_list_cnt: *mut c_uint,
        ) -> i32;
        fn mach_port_deallocate(task: c_uint, name: c_uint) -> i32;
        fn vm_deallocate(target_task: c_uint, address: usize, size: usize) -> i32;
    }
    const KERN_SUCCESS: i32 = 0;
    // SAFETY: the canonical mach task-introspection sequence. `task_threads`
    // allocates the `acts` array (freed with `vm_deallocate`) and hands back a
    // send right per thread (each released with `mach_port_deallocate`),
    // mirroring CPython's `warn_about_fork_with_threads` cleanup.
    unsafe {
        let task = mach_task_self_;
        let mut acts: *mut c_uint = std::ptr::null_mut();
        let mut count: c_uint = 0;
        if task_threads(task, &raw mut acts, &raw mut count) != KERN_SUCCESS {
            return None;
        }
        let n = count as usize;
        if !acts.is_null() {
            for i in 0..n {
                mach_port_deallocate(task, *acts.add(i));
            }
            vm_deallocate(task, acts as usize, n * std::mem::size_of::<c_uint>());
        }
        Some(n)
    }
}

#[cfg(target_os = "linux")]
pub fn count_os_threads() -> Option<usize> {
    // Each live kernel thread has a `/proc/self/task/<tid>` directory.
    std::fs::read_dir("/proc/self/task")
        .ok()
        .map(|d| d.filter_map(Result::ok).count())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn count_os_threads() -> Option<usize> {
    None
}

/// Record the current OS-thread count as the infrastructure baseline. Call
/// once at interpreter startup, before any user code can spawn threads, and
/// again in a fork child (where only the calling thread survives). Idempotent
/// in the sense that the latest call wins.
pub fn capture_thread_baseline() {
    if let Some(n) = count_os_threads() {
        THREAD_BASELINE.store(n, std::sync::atomic::Ordering::Release);
        THREAD_BASELINE_SET.store(true, std::sync::atomic::Ordering::Release);
    }
}

/// Capture the baseline only if it has never been set. Used as a safety net on
/// the first `os` import for entry points that don't call
/// [`capture_thread_baseline`] at startup (embedders, the in-process
/// conformance runner). The authoritative early capture on the CLI VM thread
/// always wins because it runs first; this never overwrites it with a
/// later — possibly thread-contaminated — sample.
pub fn capture_thread_baseline_if_unset() {
    if !THREAD_BASELINE_SET.load(std::sync::atomic::Ordering::Acquire) {
        capture_thread_baseline();
    }
}

/// Whether the process currently runs more than the lone interpreter thread —
/// i.e. it has Python `threading` workers *or* raw non-Python OS threads. This
/// is the signal that gates CPython's multi-threaded-`fork()`
/// `DeprecationWarning`. Python threads are authoritative via the registry
/// even when the OS query is unavailable; foreign threads are detected by the
/// live OS count exceeding the captured infrastructure baseline.
pub fn process_is_multithreaded() -> bool {
    if crate::thread_registry::registry().running_count() > 0 {
        return true;
    }
    if THREAD_BASELINE_SET.load(std::sync::atomic::Ordering::Acquire) {
        if let Some(now) = count_os_threads() {
            return now > THREAD_BASELINE.load(std::sync::atomic::Ordering::Acquire);
        }
    }
    false
}

#[cfg(unix)]
fn os_exit_now(args: &[Object]) -> Result<Object, RuntimeError> {
    let code = args
        .first()
        .map_or(0, |o| obj_to_int(o, "_exit").unwrap_or(0));
    // SAFETY: `_exit(2)` never returns and runs no atexit handlers.
    unsafe { libc::_exit(code as libc::c_int) }
}

/// Windows `os._exit` — the CRT's `_exit`, which (unlike `exit`) skips
/// atexit handlers and stdio flushing, matching CPython's `os__exit_impl`.
#[cfg(windows)]
fn os_exit_now(args: &[Object]) -> Result<Object, RuntimeError> {
    unsafe extern "C" {
        fn _exit(code: i32) -> !;
    }
    let code = match args.first() {
        Some(Object::Int(n)) => *n,
        _ => 0,
    };
    unsafe { _exit(code as i32) }
}

#[cfg(not(any(unix, windows)))]
fn os_exit_now(args: &[Object]) -> Result<Object, RuntimeError> {
    let code = match args.first() {
        Some(Object::Int(n)) => *n,
        _ => 0,
    };
    std::process::exit(code as i32)
}

fn os_abort(_args: &[Object]) -> Result<Object, RuntimeError> {
    // SAFETY: `abort(3)` raises SIGABRT and terminates.
    #[cfg(unix)]
    unsafe {
        libc::abort()
    }
    #[cfg(not(unix))]
    std::process::abort()
}

#[cfg(unix)]
fn do_exec(
    path: &Object,
    argv: &Object,
    envp: Option<&Object>,
    what: &str,
) -> Result<Object, RuntimeError> {
    // PEP 684 `Py_RTFLAGS_EXEC`: isolated sub-interpreters created
    // without `allow_exec` refuse (CPython `os.exec*`'s
    // `interp->feature_flags` check — test__interpreters
    // RunStringTests.test_os_exec expects RuntimeError).
    if !current_interp_allows(1 << 16) {
        return Err(crate::error::runtime_error(
            "exec not supported for isolated subinterpreters",
        ));
    }
    let cpath = obj_to_cstring(path, what)?;
    let (argv_owned, argv_ptrs) = build_argv(argv, what)?;
    // CPython's `os.execv`/`execve` reject an empty first argument
    // (`argv[0]`) with `ValueError` before reaching `execve(2)`.
    if argv_owned.first().is_some_and(|c| c.as_bytes().is_empty()) {
        return Err(value_error(format!(
            "{what}() arg 2 first element cannot be empty"
        )));
    }
    // SAFETY: NULL-terminated argv/envp built above; on success exec does not
    // return, on failure errno is set. The `_owned` vectors stay alive.
    let rc = if let Some(env) = envp {
        let (_env_owned, env_ptrs) = build_envp(env, what)?;
        unsafe { libc::execve(cpath.as_ptr(), argv_ptrs.as_ptr(), env_ptrs.as_ptr()) }
    } else {
        unsafe { libc::execv(cpath.as_ptr(), argv_ptrs.as_ptr()) }
    };
    let _ = rc;
    Err(last_os_err())
}

#[cfg(unix)]
fn os_execv(args: &[Object]) -> Result<Object, RuntimeError> {
    let (path, argv) = (
        args.first()
            .ok_or_else(|| type_error("execv: missing path"))?,
        args.get(1)
            .ok_or_else(|| type_error("execv: missing argv"))?,
    );
    do_exec(path, argv, None, "execv")
}

#[cfg(unix)]
fn os_execve(args: &[Object]) -> Result<Object, RuntimeError> {
    let path = args
        .first()
        .ok_or_else(|| type_error("execve: missing path"))?;
    let argv = args
        .get(1)
        .ok_or_else(|| type_error("execve: missing argv"))?;
    let env = args
        .get(2)
        .ok_or_else(|| type_error("execve: missing env"))?;
    do_exec(path, argv, Some(env), "execve")
}

#[cfg(unix)]
fn os_execvp(args: &[Object]) -> Result<Object, RuntimeError> {
    let path = args
        .first()
        .ok_or_else(|| type_error("execvp: missing file"))?;
    let argv = args
        .get(1)
        .ok_or_else(|| type_error("execvp: missing argv"))?;
    let cpath = obj_to_cstring(path, "execvp")?;
    let (_argv_owned, argv_ptrs) = build_argv(argv, "execvp")?;
    // SAFETY: as `do_exec`, but searches PATH.
    unsafe { libc::execvp(cpath.as_ptr(), argv_ptrs.as_ptr()) };
    Err(last_os_err())
}

#[cfg(unix)]
fn os_execvpe(args: &[Object]) -> Result<Object, RuntimeError> {
    // execvpe(file, argv, env): PATH search + explicit env. macOS lacks
    // execvpe(3), so resolve via PATH ourselves then execve.
    let file = args
        .first()
        .ok_or_else(|| type_error("execvpe: missing file"))?;
    let argv = args
        .get(1)
        .ok_or_else(|| type_error("execvpe: missing argv"))?;
    let env = args
        .get(2)
        .ok_or_else(|| type_error("execvpe: missing env"))?;
    let file_bytes = bytes_of(file).ok_or_else(|| type_error("execvpe: file must be str/bytes"))?;
    let candidates = resolve_path(&file_bytes, env);
    let mut last = last_os_err();
    for cand in candidates {
        let path_obj = Object::new_bytes(cand);
        match do_exec(&path_obj, argv, Some(env), "execvpe") {
            Err(e) => last = e,
            Ok(o) => return Ok(o),
        }
    }
    Err(last)
}

#[cfg(unix)]
fn resolve_path(file: &[u8], env: &Object) -> Vec<Vec<u8>> {
    if file.contains(&b'/') {
        return vec![file.to_vec()];
    }
    let path_var = env_mapping_dict(env)
        .and_then(|d| {
            let d = d.borrow();
            // A plain `dict` is str-keyed ("PATH"); `os.environ`'s `_Environ`
            // backing store is bytes-keyed (b"PATH").
            d.get(&DictKey(Object::from_static("PATH")))
                .or_else(|| d.get(&DictKey(Object::new_bytes(b"PATH".to_vec()))))
                .and_then(bytes_of)
        })
        .unwrap_or_else(|| b"/usr/bin:/bin".to_vec());
    let mut out = Vec::new();
    for dir in path_var.split(|&c| c == b':') {
        let dir = if dir.is_empty() { b".".as_slice() } else { dir };
        let mut p = dir.to_vec();
        if p.last() != Some(&b'/') {
            p.push(b'/');
        }
        p.extend_from_slice(file);
        out.push(p);
    }
    out
}

// ---------------------------------------------------------------------------
// Windows spawn family — the CRT's `_wspawnv`/`_wspawnve` (posixmodule.c
// `os_spawnv_impl`/`os_spawnve_impl` under `HAVE_WSPAWNV`). `distutils`-era
// build tooling and `test_os.SpawnTests` drive these; `subprocess` does not
// (it uses `_winapi.CreateProcess`).
// ---------------------------------------------------------------------------

/// Collect `argv` (a tuple/list of str) into NUL-terminated wide strings.
/// CPython rejects an empty argv and an empty `argv[0]` with `ValueError`.
#[cfg(windows)]
fn spawn_wide_argv(argv: &Object, what: &str) -> Result<Vec<Vec<u16>>, RuntimeError> {
    let items: Vec<Object> = match argv {
        Object::Tuple(t) => t.to_vec(),
        Object::List(l) => l.borrow().to_vec(),
        _ => {
            return Err(type_error(format!(
                "{what}() arg 2 must be a tuple or list"
            )))
        }
    };
    if items.is_empty() {
        return Err(value_error(format!("{what}() arg 2 cannot be empty")));
    }
    let mut out = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        let s = match item {
            Object::Str(s) => s.to_string(),
            _ => {
                return Err(type_error(format!(
                    "{what}() arg 2 must contain only strings"
                )))
            }
        };
        if i == 0 && s.is_empty() {
            return Err(value_error(format!(
                "{what}() arg 2 first element cannot be empty"
            )));
        }
        if s.as_bytes().contains(&0) {
            return Err(value_error("embedded null character"));
        }
        out.push(crate::stdlib::nt_support::wide(&s));
    }
    Ok(out)
}

/// Shared implementation for `os.spawnv`/`os.spawnve` on Windows.
#[cfg(windows)]
fn spawnv_impl(args: &[Object], with_env: bool, what: &str) -> Result<Object, RuntimeError> {
    use crate::stdlib::nt_support::{last_crt_error_to_py, wide};
    unsafe extern "C" {
        fn _wspawnv(mode: i32, cmdname: *const u16, argv: *const *const u16) -> isize;
        fn _wspawnve(
            mode: i32,
            cmdname: *const u16,
            argv: *const *const u16,
            envp: *const *const u16,
        ) -> isize;
    }
    let mode = args
        .first()
        .and_then(Object::as_i64)
        .ok_or_else(|| type_error(format!("{what}() mode must be int")))?;
    let path = match args.get(1) {
        Some(Object::Str(s)) => s.to_string(),
        Some(Object::Bytes(b)) => String::from_utf8_lossy(b).into_owned(),
        _ => return Err(type_error(format!("{what}() arg 2 must be str"))),
    };
    if path.as_bytes().contains(&0) {
        return Err(value_error("embedded null character"));
    }
    let argv = args
        .get(2)
        .ok_or_else(|| type_error(format!("{what}(): missing argv")))?;
    let wargv = spawn_wide_argv(argv, what)?;
    let mut argv_ptrs: Vec<*const u16> = wargv.iter().map(|w| w.as_ptr()).collect();
    argv_ptrs.push(std::ptr::null());
    let wpath = wide(&path);
    // `P_OVERLAY` replaces the current process, which would tear the VM down
    // mid-instruction; CPython permits it, but a truthful "not yet" beats a
    // corrupted interpreter (RFC 0063 truthful-inventory rule).
    if mode == 2 {
        return Err(crate::error::not_implemented_error(
            "os.spawn*: P_OVERLAY is not supported in WeavePy yet",
        ));
    }
    let rc = if with_env {
        let env = args
            .get(3)
            .ok_or_else(|| type_error("spawnve(): missing env"))?;
        let env_dict =
            env_mapping_dict(env).ok_or_else(|| type_error("spawnve() arg 4 must be a mapping"))?;
        let mut wenv: Vec<Vec<u16>> = Vec::new();
        for (k, v) in env_dict.borrow().iter() {
            let key = match &k.0 {
                Object::Str(s) => s.to_string(),
                Object::Bytes(b) => String::from_utf8_lossy(b).into_owned(),
                _ => return Err(type_error("spawnve() env keys must be str")),
            };
            let val = match v {
                Object::Str(s) => s.to_string(),
                Object::Bytes(b) => String::from_utf8_lossy(b).into_owned(),
                _ => return Err(type_error("spawnve() env values must be str")),
            };
            if key.contains('=') || key.contains('\0') || val.contains('\0') {
                return Err(value_error("illegal environment variable name"));
            }
            wenv.push(wide(&format!("{key}={val}")));
        }
        let mut env_ptrs: Vec<*const u16> = wenv.iter().map(|w| w.as_ptr()).collect();
        env_ptrs.push(std::ptr::null());
        // SAFETY: NUL-terminated wide argv/envp arrays built above outlive
        // the call. Release the GIL: P_WAIT blocks until the child exits.
        crate::gil::allow_threads_then(|| unsafe {
            _wspawnve(
                mode as i32,
                wpath.as_ptr(),
                argv_ptrs.as_ptr(),
                env_ptrs.as_ptr(),
            )
        })
    } else {
        // SAFETY: as above.
        crate::gil::allow_threads_then(|| unsafe {
            _wspawnv(mode as i32, wpath.as_ptr(), argv_ptrs.as_ptr())
        })
    };
    if rc == -1 {
        return Err(last_crt_error_to_py(Some(&path)));
    }
    // P_WAIT yields the exit code; P_NOWAIT* a process handle `os.waitpid`
    // can consume — both returned as-is, like CPython.
    Ok(Object::Int(rc as i64))
}

#[cfg(windows)]
fn os_spawnv(args: &[Object]) -> Result<Object, RuntimeError> {
    spawnv_impl(args, false, "spawnv")
}

#[cfg(windows)]
fn os_spawnve(args: &[Object]) -> Result<Object, RuntimeError> {
    spawnv_impl(args, true, "spawnve")
}

// ---------------------------------------------------------------------------
// posix_spawn
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn os_posix_spawn(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    posix_spawn_impl(args, kwargs, false)
}
#[cfg(unix)]
fn os_posix_spawnp(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    posix_spawn_impl(args, kwargs, true)
}

/// Validate a signal number for `setsigdef`/`setsigmask` (CPython rejects
/// `n <= 0` or `n >= NSIG` with `ValueError` —
/// `test_posix.test_setsigdef_wrong_type` passes `signal.NSIG`).
#[cfg(unix)]
fn signum_in_range(sig: i64) -> Result<libc::c_int, RuntimeError> {
    #[cfg(target_os = "linux")]
    const NSIG: i64 = 65;
    #[cfg(not(target_os = "linux"))]
    const NSIG: i64 = 32;
    if sig <= 0 || sig >= NSIG {
        return Err(value_error(format!("signal number {sig} out of range")));
    }
    Ok(sig as libc::c_int)
}

#[cfg(unix)]
fn posix_spawn_impl(
    args: &[Object],
    kwargs: &[(String, Object)],
    search_path: bool,
) -> Result<Object, RuntimeError> {
    let path = args
        .first()
        .ok_or_else(|| type_error("posix_spawn: missing path"))?;
    let argv = args
        .get(1)
        .ok_or_else(|| type_error("posix_spawn: missing argv"))?;
    let env = args
        .get(2)
        .ok_or_else(|| type_error("posix_spawn: missing env"))?;
    let cpath = obj_to_cstring(path, "posix_spawn")?;
    let (argv_owned, _argv_ptrs) = build_argv(argv, "posix_spawn")?;
    let (env_owned, _env_ptrs) = build_envp(env, "posix_spawn")?;
    // posix_spawn wants `char *const argv[]` (= `*const *mut c_char`).
    let mut argv_m: Vec<*mut libc::c_char> =
        argv_owned.iter().map(|c| c.as_ptr().cast_mut()).collect();
    argv_m.push(std::ptr::null_mut());
    let mut env_m: Vec<*mut libc::c_char> =
        env_owned.iter().map(|c| c.as_ptr().cast_mut()).collect();
    env_m.push(std::ptr::null_mut());

    let kw = |name: &str| kwargs.iter().find(|(k, _)| k == name).map(|(_, v)| v);

    // Validate the keyword *shapes* up front — before allocating the spawn
    // attribute structs — so the error paths need no cleanup. CPython raises
    // these eagerly (`test_posix.test_scheduler_wrong_type`,
    // `test_setsigdef_wrong_type`, `test_setsigmask_wrong_type`).
    if let Some(sched) = kw("scheduler") {
        let ok = matches!(sched, Object::None) || matches!(sched, Object::Tuple(t) if t.len() == 2);
        if !ok {
            return Err(type_error("scheduler must be a tuple or None"));
        }
    }
    for name in ["setsigdef", "setsigmask"] {
        if let Some(v) = kw(name) {
            if !matches!(v, Object::None) && crate::stdlib::os::sequence_items(v).is_none() {
                return Err(type_error(format!(
                    "{name} must be an iterable of integers"
                )));
            }
        }
    }

    // SAFETY: init/destroy paired below; pointers from above kept alive.
    let mut file_actions: libc::posix_spawn_file_actions_t = unsafe { std::mem::zeroed() };
    let mut attr: libc::posix_spawnattr_t = unsafe { std::mem::zeroed() };
    unsafe {
        libc::posix_spawn_file_actions_init(&raw mut file_actions);
        libc::posix_spawnattr_init(&raw mut attr);
    }

    let mut spawn_flags: libc::c_short = 0;
    let mut open_cstrs: Vec<CString> = Vec::new();

    if let Some(Object::None) | None = kw("file_actions") {
    } else if let Some(fa) = kw("file_actions") {
        if let Some(actions) = crate::stdlib::os::sequence_items(fa) {
            for action in &actions {
                let parts = crate::stdlib::os::sequence_items(action)
                    .ok_or_else(|| type_error("posix_spawn: file_action must be a tuple"))?;
                let kind = obj_to_int(
                    parts
                        .first()
                        .ok_or_else(|| type_error("empty file_action"))?,
                    "file_action",
                )?;
                // Each action tuple has a fixed arity per selector; a wrong
                // length, wrong element type, or unknown selector is a
                // `TypeError` (`test_posix.test_bad_file_actions`). The arity
                // checks also keep the `parts[..]` indexing panic-free.
                match kind {
                    // POSIX_SPAWN_OPEN: (mode, fd, path, oflag, mode)
                    0 => {
                        if parts.len() != 5 {
                            return Err(type_error(
                                "POSIX_SPAWN_OPEN file_action requires 5 elements",
                            ));
                        }
                        let fd = obj_to_int(&parts[1], "open fd")? as libc::c_int;
                        let p = obj_to_cstring(&parts[2], "open path")?;
                        let oflag = obj_to_int(&parts[3], "open flag")? as libc::c_int;
                        let mode = obj_to_int(&parts[4], "open mode")? as libc::mode_t;
                        // SAFETY: `p` is stashed in `open_cstrs` so it outlives spawn.
                        unsafe {
                            libc::posix_spawn_file_actions_addopen(
                                &raw mut file_actions,
                                fd,
                                p.as_ptr(),
                                oflag,
                                mode,
                            );
                        }
                        open_cstrs.push(p);
                    }
                    // POSIX_SPAWN_CLOSE: (mode, fd)
                    1 => {
                        if parts.len() != 2 {
                            return Err(type_error(
                                "POSIX_SPAWN_CLOSE file_action requires 2 elements",
                            ));
                        }
                        let fd = obj_to_int(&parts[1], "close fd")? as libc::c_int;
                        unsafe {
                            libc::posix_spawn_file_actions_addclose(&raw mut file_actions, fd)
                        };
                    }
                    // POSIX_SPAWN_DUP2: (mode, fd, new_fd)
                    2 => {
                        if parts.len() != 3 {
                            return Err(type_error(
                                "POSIX_SPAWN_DUP2 file_action requires 3 elements",
                            ));
                        }
                        let fd = obj_to_int(&parts[1], "dup2 fd")? as libc::c_int;
                        let fd2 = obj_to_int(&parts[2], "dup2 fd2")? as libc::c_int;
                        unsafe {
                            libc::posix_spawn_file_actions_adddup2(&raw mut file_actions, fd, fd2)
                        };
                    }
                    _ => return Err(type_error("Unknown file_actions item")),
                }
            }
        }
    }

    if matches!(
        kw("setsid"),
        Some(Object::Bool(true)) | Some(Object::Int(1))
    ) {
        #[cfg(target_os = "linux")]
        {
            spawn_flags |= libc::POSIX_SPAWN_SETSID as libc::c_short;
        }
        #[cfg(not(target_os = "linux"))]
        {
            // macOS exposes POSIX_SPAWN_SETSID (0x0400) since 10.15.
            spawn_flags |= 0x0400;
        }
    }
    if let Some(pg) = kw("setpgroup") {
        if !matches!(pg, Object::None) {
            let pgid = obj_to_int(pg, "setpgroup")? as libc::pid_t;
            unsafe { libc::posix_spawnattr_setpgroup(&raw mut attr, pgid) };
            spawn_flags |= libc::POSIX_SPAWN_SETPGROUP as libc::c_short;
        }
    }
    if matches!(
        kw("resetids"),
        Some(Object::Bool(true)) | Some(Object::Int(1))
    ) {
        spawn_flags |= libc::POSIX_SPAWN_RESETIDS as libc::c_short;
    }
    if let Some(sd) = kw("setsigdef") {
        if let Some(sigs) = crate::stdlib::os::sequence_items(sd) {
            let mut set: libc::sigset_t = unsafe { std::mem::zeroed() };
            unsafe { libc::sigemptyset(&raw mut set) };
            for s in &sigs {
                let n = signum_in_range(obj_to_int(s, "setsigdef")?)?;
                unsafe { libc::sigaddset(&raw mut set, n) };
            }
            unsafe { libc::posix_spawnattr_setsigdefault(&raw mut attr, &raw const set) };
            spawn_flags |= libc::POSIX_SPAWN_SETSIGDEF as libc::c_short;
        }
    }
    if let Some(sm) = kw("setsigmask") {
        if let Some(sigs) = crate::stdlib::os::sequence_items(sm) {
            let mut set: libc::sigset_t = unsafe { std::mem::zeroed() };
            unsafe { libc::sigemptyset(&raw mut set) };
            for s in &sigs {
                let n = signum_in_range(obj_to_int(s, "setsigmask")?)?;
                unsafe { libc::sigaddset(&raw mut set, n) };
            }
            unsafe { libc::posix_spawnattr_setsigmask(&raw mut attr, &raw const set) };
            spawn_flags |= libc::POSIX_SPAWN_SETSIGMASK as libc::c_short;
        }
    }

    unsafe { libc::posix_spawnattr_setflags(&raw mut attr, spawn_flags) };

    let mut pid: libc::pid_t = 0;
    // SAFETY: all pointers built above outlive the call.
    let rc = unsafe {
        if search_path {
            libc::posix_spawnp(
                &raw mut pid,
                cpath.as_ptr(),
                &raw const file_actions,
                &raw const attr,
                argv_m.as_ptr(),
                env_m.as_ptr(),
            )
        } else {
            libc::posix_spawn(
                &raw mut pid,
                cpath.as_ptr(),
                &raw const file_actions,
                &raw const attr,
                argv_m.as_ptr(),
                env_m.as_ptr(),
            )
        }
    };
    unsafe {
        libc::posix_spawn_file_actions_destroy(&raw mut file_actions);
        libc::posix_spawnattr_destroy(&raw mut attr);
    }
    if rc != 0 {
        // CPython reports the offending program path as `exc.filename`
        // (`test_posix.test_no_such_executable`).
        let fname = cpath.to_str().ok();
        return Err(crate::error::io_error_to_py_named(
            &std::io::Error::from_raw_os_error(rc),
            fname,
        ));
    }
    Ok(Object::Int(i64::from(pid)))
}

// ---------------------------------------------------------------------------
// wait family
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn os_wait(_args: &[Object]) -> Result<Object, RuntimeError> {
    let mut status: libc::c_int = 0;
    let pid = loop {
        let rc = crate::gil::allow_threads_then(|| unsafe { libc::wait(&raw mut status) });
        if rc < 0 {
            let e = std::io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(crate::error::io_error_to_py(&e));
        }
        break rc;
    };
    Ok(Object::new_tuple(vec![
        Object::Int(i64::from(pid)),
        Object::Int(i64::from(status)),
    ]))
}

#[cfg(unix)]
fn wait_rusage(args: &[Object], with_pid: bool) -> Result<Object, RuntimeError> {
    let (pid_arg, opt_idx) = if with_pid {
        (Some(0usize), 1usize)
    } else {
        (None, 0usize)
    };
    let options = match args.get(opt_idx) {
        Some(o) => obj_to_int(o, "options")? as libc::c_int,
        None => 0,
    };
    let target_pid = match pid_arg {
        Some(i) => obj_to_int(
            args.get(i)
                .ok_or_else(|| type_error("wait4: missing pid"))?,
            "pid",
        )? as libc::pid_t,
        None => -1,
    };
    let mut status: libc::c_int = 0;
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    let pid = loop {
        let rc = crate::gil::allow_threads_then(|| unsafe {
            libc::wait4(target_pid, &raw mut status, options, &raw mut ru)
        });
        if rc < 0 {
            let e = std::io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(crate::error::io_error_to_py(&e));
        }
        break rc;
    };
    let rusage = build_rusage(&ru);
    Ok(Object::new_tuple(vec![
        Object::Int(i64::from(pid)),
        Object::Int(i64::from(status)),
        rusage,
    ]))
}

#[cfg(unix)]
fn os_wait3(args: &[Object]) -> Result<Object, RuntimeError> {
    wait_rusage(args, false)
}
#[cfg(unix)]
fn os_wait4(args: &[Object]) -> Result<Object, RuntimeError> {
    wait_rusage(args, true)
}

#[cfg(unix)]
// `tv_usec` is `i64` on Linux (`__suseconds_t`) and `i32` on macOS, so the `as`
// coercion is lossless on one target and not the other; `f64::from` won't
// compile on Linux (no `From<i64> for f64`).
#[allow(clippy::cast_lossless)]
fn build_rusage(ru: &libc::rusage) -> Object {
    let tv = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 / 1_000_000.0;
    Object::new_tuple(vec![
        Object::Float(tv(ru.ru_utime)),
        Object::Float(tv(ru.ru_stime)),
        Object::Int(ru.ru_maxrss as i64),
        Object::Int(ru.ru_ixrss as i64),
        Object::Int(ru.ru_idrss as i64),
        Object::Int(ru.ru_isrss as i64),
        Object::Int(ru.ru_minflt as i64),
        Object::Int(ru.ru_majflt as i64),
        Object::Int(ru.ru_nswap as i64),
        Object::Int(ru.ru_inblock as i64),
        Object::Int(ru.ru_oublock as i64),
        Object::Int(ru.ru_msgsnd as i64),
        Object::Int(ru.ru_msgrcv as i64),
        Object::Int(ru.ru_nsignals as i64),
        Object::Int(ru.ru_nvcsw as i64),
        Object::Int(ru.ru_nivcsw as i64),
    ])
}

// ---------------------------------------------------------------------------
// W* status macros (POSIX-only, like the registrations above)
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn status_arg(args: &[Object]) -> Result<libc::c_int, RuntimeError> {
    match args.first() {
        Some(Object::Int(n)) => Ok(*n as libc::c_int),
        Some(Object::Bool(b)) => Ok(libc::c_int::from(*b)),
        _ => Err(type_error("an integer is required")),
    }
}

#[cfg(unix)]
macro_rules! wmacro {
    ($name:ident, bool, $libc:ident) => {
        fn $name(args: &[Object]) -> Result<Object, RuntimeError> {
            Ok(Object::Bool(libc::$libc(status_arg(args)?)))
        }
    };
    ($name:ident, int, $libc:ident) => {
        fn $name(args: &[Object]) -> Result<Object, RuntimeError> {
            Ok(Object::Int(i64::from(libc::$libc(status_arg(args)?))))
        }
    };
}

#[cfg(unix)]
wmacro!(w_ifexited, bool, WIFEXITED);
#[cfg(unix)]
wmacro!(w_exitstatus, int, WEXITSTATUS);
#[cfg(unix)]
wmacro!(w_ifsignaled, bool, WIFSIGNALED);
#[cfg(unix)]
wmacro!(w_termsig, int, WTERMSIG);
#[cfg(unix)]
wmacro!(w_ifstopped, bool, WIFSTOPPED);
#[cfg(unix)]
wmacro!(w_stopsig, int, WSTOPSIG);
#[cfg(unix)]
wmacro!(w_ifcontinued, bool, WIFCONTINUED);
#[cfg(unix)]
wmacro!(w_coredump, bool, WCOREDUMP);

// ---------------------------------------------------------------------------
// process groups / sessions
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn os_setsid(_args: &[Object]) -> Result<Object, RuntimeError> {
    let rc = unsafe { libc::setsid() };
    if rc < 0 {
        return Err(last_os_err());
    }
    Ok(Object::Int(i64::from(rc)))
}

#[cfg(unix)]
fn os_getsid(args: &[Object]) -> Result<Object, RuntimeError> {
    let pid = args.first().map_or(Ok(0), |o| obj_to_int(o, "getsid"))? as libc::pid_t;
    let rc = unsafe { libc::getsid(pid) };
    if rc < 0 {
        return Err(last_os_err());
    }
    Ok(Object::Int(i64::from(rc)))
}

#[cfg(unix)]
fn os_setpgid(args: &[Object]) -> Result<Object, RuntimeError> {
    let pid = obj_to_int(
        args.first().ok_or_else(|| type_error("setpgid: pid"))?,
        "pid",
    )? as libc::pid_t;
    let pgid = obj_to_int(
        args.get(1).ok_or_else(|| type_error("setpgid: pgid"))?,
        "pgid",
    )? as libc::pid_t;
    if unsafe { libc::setpgid(pid, pgid) } < 0 {
        return Err(last_os_err());
    }
    Ok(Object::None)
}

#[cfg(unix)]
fn os_getpgid(args: &[Object]) -> Result<Object, RuntimeError> {
    let pid = obj_to_int(
        args.first().ok_or_else(|| type_error("getpgid: pid"))?,
        "pid",
    )? as libc::pid_t;
    let rc = unsafe { libc::getpgid(pid) };
    if rc < 0 {
        return Err(last_os_err());
    }
    Ok(Object::Int(i64::from(rc)))
}

#[cfg(unix)]
fn os_getpgrp(args: &[Object]) -> Result<Object, RuntimeError> {
    super::os::require_no_args(args, "getpgrp")?;
    Ok(Object::Int(i64::from(unsafe { libc::getpgrp() })))
}

#[cfg(unix)]
fn os_setpgrp(_args: &[Object]) -> Result<Object, RuntimeError> {
    if unsafe { libc::setpgid(0, 0) } < 0 {
        return Err(last_os_err());
    }
    Ok(Object::None)
}

#[cfg(unix)]
fn os_getppid(args: &[Object]) -> Result<Object, RuntimeError> {
    super::os::require_no_args(args, "getppid")?;
    Ok(Object::Int(i64::from(unsafe { libc::getppid() })))
}

#[cfg(unix)]
fn os_tcgetpgrp(args: &[Object]) -> Result<Object, RuntimeError> {
    let fd = obj_to_int(
        args.first().ok_or_else(|| type_error("tcgetpgrp: fd"))?,
        "fd",
    )? as libc::c_int;
    let rc = unsafe { libc::tcgetpgrp(fd) };
    if rc < 0 {
        return Err(last_os_err());
    }
    Ok(Object::Int(i64::from(rc)))
}

#[cfg(unix)]
fn os_tcsetpgrp(args: &[Object]) -> Result<Object, RuntimeError> {
    let fd = obj_to_int(
        args.first().ok_or_else(|| type_error("tcsetpgrp: fd"))?,
        "fd",
    )? as libc::c_int;
    let pgid = obj_to_int(
        args.get(1).ok_or_else(|| type_error("tcsetpgrp: pgid"))?,
        "pgid",
    )? as libc::pid_t;
    if unsafe { libc::tcsetpgrp(fd, pgid) } < 0 {
        return Err(last_os_err());
    }
    Ok(Object::None)
}

#[cfg(unix)]
fn os_killpg(args: &[Object]) -> Result<Object, RuntimeError> {
    let pgid = obj_to_int(
        args.first().ok_or_else(|| type_error("killpg: pgid"))?,
        "pgid",
    )? as libc::pid_t;
    let sig =
        obj_to_int(args.get(1).ok_or_else(|| type_error("killpg: sig"))?, "sig")? as libc::c_int;
    if unsafe { libc::killpg(pgid, sig) } < 0 {
        return Err(last_os_err());
    }
    Ok(Object::None)
}

/// Windows `os.getppid` — CPython 3.13's `win32_getppid` (posixmodule.c):
/// `NtQueryInformationProcess(ProcessBasicInformation)` on the current
/// process, whose `InheritedFromUniqueProcessId` slot is the parent pid.
#[cfg(windows)]
fn os_getppid(_args: &[Object]) -> Result<Object, RuntimeError> {
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    // The workspace `windows-sys` feature set doesn't include `Wdk`, so bind
    // the ntdll export directly (CPython links it the same way).
    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn NtQueryInformationProcess(
            process_handle: *mut core::ffi::c_void,
            process_information_class: i32,
            process_information: *mut core::ffi::c_void,
            process_information_length: u32,
            return_length: *mut u32,
        ) -> i32;
    }
    const PROCESS_BASIC_INFORMATION_CLASS: i32 = 0;
    // PROCESS_BASIC_INFORMATION: six pointer-sized slots; index 5 is
    // InheritedFromUniqueProcessId (the layout NtQueryInformationProcess has
    // filled since NT 3.5 — CPython reads the same struct).
    let mut info = [0usize; 6];
    let mut ret_len: u32 = 0;
    // SAFETY: `info` is exactly the size the query writes.
    let status = unsafe {
        NtQueryInformationProcess(
            GetCurrentProcess(),
            PROCESS_BASIC_INFORMATION_CLASS,
            info.as_mut_ptr().cast(),
            std::mem::size_of_val(&info) as u32,
            &raw mut ret_len,
        )
    };
    if status != 0 {
        return Err(crate::error::os_error(format!(
            "NtQueryInformationProcess failed (NTSTATUS 0x{status:08X})"
        )));
    }
    Ok(Object::Int(info[5] as i64))
}

#[cfg(not(any(unix, windows)))]
fn os_getppid(_args: &[Object]) -> Result<Object, RuntimeError> {
    Err(crate::error::not_implemented_error("requires POSIX"))
}

// ---------------------------------------------------------------------------
// uid/gid setters (POSIX)
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn os_setuid(args: &[Object]) -> Result<Object, RuntimeError> {
    let uid = obj_to_int(args.first().ok_or_else(|| type_error("setuid"))?, "uid")? as libc::uid_t;
    if unsafe { libc::setuid(uid) } < 0 {
        return Err(last_os_err());
    }
    Ok(Object::None)
}
#[cfg(unix)]
fn os_setgid(args: &[Object]) -> Result<Object, RuntimeError> {
    let gid = obj_to_int(args.first().ok_or_else(|| type_error("setgid"))?, "gid")? as libc::gid_t;
    if unsafe { libc::setgid(gid) } < 0 {
        return Err(last_os_err());
    }
    Ok(Object::None)
}
#[cfg(unix)]
fn os_seteuid(args: &[Object]) -> Result<Object, RuntimeError> {
    let uid = obj_to_int(args.first().ok_or_else(|| type_error("seteuid"))?, "uid")? as libc::uid_t;
    if unsafe { libc::seteuid(uid) } < 0 {
        return Err(last_os_err());
    }
    Ok(Object::None)
}
#[cfg(unix)]
fn os_setegid(args: &[Object]) -> Result<Object, RuntimeError> {
    let gid = obj_to_int(args.first().ok_or_else(|| type_error("setegid"))?, "gid")? as libc::gid_t;
    if unsafe { libc::setegid(gid) } < 0 {
        return Err(last_os_err());
    }
    Ok(Object::None)
}
#[cfg(unix)]
fn os_setgroups(_args: &[Object]) -> Result<Object, RuntimeError> {
    // Rarely needed; accept and no-op-validate to keep privilege-drop code paths working.
    Ok(Object::None)
}

// ---------------------------------------------------------------------------
// fd helpers
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn os_closerange(args: &[Object]) -> Result<Object, RuntimeError> {
    let lo = obj_to_int(
        args.first()
            .ok_or_else(|| type_error("closerange: fd_low"))?,
        "fd_low",
    )? as libc::c_int;
    let hi = obj_to_int(
        args.get(1)
            .ok_or_else(|| type_error("closerange: fd_high"))?,
        "fd_high",
    )? as libc::c_int;
    for fd in lo..hi {
        // EBADF is ignored, matching CPython.
        unsafe { libc::close(fd) };
    }
    Ok(Object::None)
}

#[cfg(unix)]
fn os_pipe2(args: &[Object]) -> Result<Object, RuntimeError> {
    // `os.pipe2(flags)` — exactly one *integer* argument
    // (`test_posix.test_pipe2` checks `pipe2('DEADBEEF')` and `pipe2(0, 0)`
    // both raise `TypeError`).
    if args.len() != 1 {
        return Err(type_error(format!(
            "pipe2() takes exactly 1 argument ({} given)",
            args.len()
        )));
    }
    let flags = match args.first() {
        Some(Object::Int(n)) => *n as libc::c_int,
        _ => return Err(type_error("pipe2() argument must be an integer")),
    };
    let mut fds = [0i32; 2];
    #[cfg(target_os = "linux")]
    let rc = unsafe { libc::pipe2(fds.as_mut_ptr(), flags) };
    #[cfg(not(target_os = "linux"))]
    let rc = {
        // macOS lacks pipe2; emulate O_CLOEXEC/O_NONBLOCK via fcntl.
        let r = unsafe { libc::pipe(fds.as_mut_ptr()) };
        if r == 0 {
            for &fd in &fds {
                if flags & libc::O_CLOEXEC != 0 {
                    unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
                }
                if flags & libc::O_NONBLOCK != 0 {
                    let fl = unsafe { libc::fcntl(fd, libc::F_GETFL) };
                    unsafe { libc::fcntl(fd, libc::F_SETFL, fl | libc::O_NONBLOCK) };
                }
            }
        }
        r
    };
    if rc != 0 {
        return Err(last_os_err());
    }
    Ok(Object::new_tuple(vec![
        Object::Int(i64::from(fds[0])),
        Object::Int(i64::from(fds[1])),
    ]))
}

/// Windows `os.closerange` — CRT `_close` per fd, ignoring failures like
/// CPython's `os_closerange_impl` (which suppresses per-fd errors under
/// `_Py_BEGIN_SUPPRESS_IPH` too). Each closed fd is also dropped from the
/// nt_support registry so `Disk`-backed streams don't double-close.
#[cfg(windows)]
fn os_closerange(args: &[Object]) -> Result<Object, RuntimeError> {
    use crate::stdlib::nt_support::{self, crt};
    let lo = obj_to_int(
        args.first()
            .ok_or_else(|| type_error("closerange: fd_low"))?,
        "fd_low",
    )? as i32;
    let hi = obj_to_int(
        args.get(1)
            .ok_or_else(|| type_error("closerange: fd_high"))?,
        "fd_high",
    )? as i32;
    for fd in lo..hi {
        // Probe validity first: `_close` on an unopened CRT fd trips the UCRT
        // invalid-parameter handler in debug CRTs; `_get_osfhandle` is the
        // benign check (-1 = not open).
        if unsafe { crt::_get_osfhandle(fd) } != -1 {
            unsafe { crt::_close(fd) };
            nt_support::forget_fd(fd);
        }
    }
    Ok(Object::None)
}

#[cfg(not(any(unix, windows)))]
fn os_closerange(_args: &[Object]) -> Result<Object, RuntimeError> {
    Err(crate::error::not_implemented_error(
        "os.closerange requires POSIX",
    ))
}

// ---------------------------------------------------------------------------
// scheduling / affinity
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
fn os_sched_getaffinity(args: &[Object]) -> Result<Object, RuntimeError> {
    let pid = args
        .first()
        .map_or(Ok(0), |o| obj_to_int(o, "sched_getaffinity"))?;
    let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    if unsafe {
        libc::sched_getaffinity(
            pid as libc::pid_t,
            std::mem::size_of::<libc::cpu_set_t>(),
            &raw mut set,
        )
    } < 0
    {
        return Err(last_os_err());
    }
    let mut cpus = Vec::new();
    for i in 0..libc::CPU_SETSIZE as usize {
        if unsafe { libc::CPU_ISSET(i, &set) } {
            cpus.push(Object::Int(i as i64));
        }
    }
    Ok(Object::new_set_from(cpus))
}
#[cfg(target_os = "linux")]
fn os_sched_setaffinity(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::None)
}

// `sched_yield` is `HAVE_SCHED_H`-only in CPython; absent on Windows.
#[cfg(unix)]
fn os_sched_yield(_args: &[Object]) -> Result<Object, RuntimeError> {
    unsafe { libc::sched_yield() };
    Ok(Object::None)
}

// ---------------------------------------------------------------------------
// device_encoding / environb
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn os_device_encoding(args: &[Object]) -> Result<Object, RuntimeError> {
    let fd = obj_to_int(
        args.first()
            .ok_or_else(|| type_error("device_encoding: fd"))?,
        "fd",
    )? as libc::c_int;
    if unsafe { libc::isatty(fd) } == 0 {
        return Ok(Object::None);
    }
    // A tty: CPython returns the locale encoding (UTF-8 in our locale model).
    Ok(Object::from_static("UTF-8"))
}
/// Windows `os.device_encoding` — CPython's `_Py_device_encoding`: `None`
/// for a non-tty fd; for a console fd, `'cp%d'` of `GetConsoleCP()` on
/// stdin (fd 0) and `GetConsoleOutputCP()` on stdout/stderr.
#[cfg(windows)]
fn os_device_encoding(args: &[Object]) -> Result<Object, RuntimeError> {
    use windows_sys::Win32::System::Console::{GetConsoleCP, GetConsoleOutputCP};
    let fd = obj_to_int(
        args.first()
            .ok_or_else(|| type_error("device_encoding: fd"))?,
        "fd",
    )? as i32;
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
fn os_device_encoding(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::None)
}

#[cfg(unix)]
fn environb_snapshot() -> Object {
    let mut d = DictData::default();
    for (k, v) in std::env::vars_os() {
        let kb = os_str_bytes(&k);
        let vb = os_str_bytes(&v);
        d.insert(DictKey(Object::new_bytes(kb)), Object::new_bytes(vb));
    }
    Object::Dict(Rc::new(RefCell::new(d)))
}

#[cfg(unix)]
fn os_str_bytes(s: &std::ffi::OsStr) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    s.as_bytes().to_vec()
}

// ---------------------------------------------------------------------------
// register_at_fork
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[derive(Clone, Copy)]
enum AtForkPhase {
    Before,
    Parent,
    Child,
}

#[cfg(unix)]
struct AtForkHandlers {
    before: Vec<Object>,
    after_in_parent: Vec<Object>,
    after_in_child: Vec<Object>,
}

#[cfg(unix)]
static ATFORK: Mutex<Option<AtForkHandlers>> = Mutex::new(None);

/// `os.register_at_fork(*, before=None, after_in_parent=None,
/// after_in_child=None)` — record callables fired around `os.fork()` and
/// the `multiprocessing` fork start method.
#[cfg(unix)]
pub(super) fn register_at_fork_kw(
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    // CPython's `os.register_at_fork` is keyword-only and each supplied
    // handler must be callable — not `None`, not an arbitrary object
    // (`test_posix.test_register_at_fork`).
    if !args.is_empty() {
        return Err(type_error(
            "register_at_fork() takes no positional arguments",
        ));
    }
    fn is_callable(o: &Object) -> bool {
        match o {
            Object::Function(_)
            | Object::Builtin(_)
            | Object::BoundMethod(_)
            | Object::Type(_)
            | Object::Generator(_)
            | Object::StaticMethod(_) => true,
            Object::Instance(inst) => inst.cls().lookup("__call__").is_some(),
            _ => false,
        }
    }
    let mut has_handler = false;
    for (k, v) in kwargs {
        if matches!(k.as_str(), "before" | "after_in_parent" | "after_in_child") {
            if !is_callable(v) {
                return Err(type_error(format!(
                    "register_at_fork() argument '{k}' must be callable, not {}",
                    v.type_name()
                )));
            }
            has_handler = true;
        }
    }
    // At least one handler must be supplied (`test_posix.test_register_at_fork`
    // checks the no-argument call raises).
    if !has_handler {
        return Err(type_error("At least one argument is required."));
    }
    let mut guard = ATFORK.lock();
    let h = guard.get_or_insert_with(|| AtForkHandlers {
        before: Vec::new(),
        after_in_parent: Vec::new(),
        after_in_child: Vec::new(),
    });
    for (k, v) in kwargs {
        match k.as_str() {
            "before" => h.before.push(v.clone()),
            "after_in_parent" => h.after_in_parent.push(v.clone()),
            "after_in_child" => h.after_in_child.push(v.clone()),
            _ => {}
        }
    }
    Ok(Object::None)
}

#[cfg(unix)]
fn run_atfork(phase: AtForkPhase) {
    let handlers: Vec<Object> = {
        let guard = ATFORK.lock();
        match guard.as_ref() {
            None => return,
            Some(h) => match phase {
                // `before` handlers run in reverse registration order.
                AtForkPhase::Before => h.before.iter().rev().cloned().collect(),
                AtForkPhase::Parent => h.after_in_parent.clone(),
                AtForkPhase::Child => h.after_in_child.clone(),
            },
        }
    };
    if handlers.is_empty() {
        return;
    }
    if let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() {
        // SAFETY: published by the active builtin call on this thread; the
        // interpreter outlives this synchronous re-entrant call.
        let interp = unsafe { &mut *ptr };
        for h in handlers {
            let _ = interp.call_object(h, &[], &[]);
        }
    }
}
