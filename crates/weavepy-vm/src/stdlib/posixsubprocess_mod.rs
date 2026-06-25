//! `_posixsubprocess` — the CPython-faithful `fork_exec` primitive (RFC
//! 0040 WS2) behind the verbatim `subprocess.Popen` driver.
//!
//! `fork_exec(...)` forks and, in the child, performs only
//! async-signal-safe work: it dups the pipe ends onto stdin/stdout/
//! stderr, restores signal dispositions, optionally `setsid`/`setpgid`/
//! drops privileges/`chdir`s, runs an optional `preexec_fn`, closes the
//! inherited fds (honouring `fds_to_keep`), then walks the candidate
//! executables calling `execv(e)`. Any failure is reported to the parent
//! through `errpipe_write` as `b"<ExcName>:<hexerrno>:<msg>"` (the format
//! `subprocess._execute_child` parses) and the child `_exit`s.
//!
//! Tracks CPython 3.13's `Modules/_posixsubprocess.c`.

#[cfg(unix)]
use crate::error::type_error;
use crate::error::RuntimeError;
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};
use crate::sync::Rc;
use crate::sync::RefCell;

#[cfg(unix)]
use std::ffi::CString;

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("_posixsubprocess"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("CPython-faithful fork+exec primitive (Rust core)."),
        );
        d.insert(
            DictKey(Object::from_static("fork_exec")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "fork_exec",
                binds_instance: false,
                call: Box::new(fork_exec),
                call_kw: None,
            })),
        );
    }
    Rc::new(PyModule {
        name: "_posixsubprocess".to_owned(),
        filename: None,
        dict,
    })
}

// ---------------------------------------------------------------------------

#[cfg(unix)]
fn obj_bytes(o: &Object) -> Option<Vec<u8>> {
    match o {
        Object::Str(s) => Some(s.as_bytes().to_vec()),
        // PEP 383: a surrogate-bearing `str` (args/executable/env entry) is
        // encoded with the filesystem codec (UTF-8) + `surrogateescape`, so a
        // value round-tripped from an undecodable env byte (0x80..0xFF →
        // U+DC80..U+DCFF) re-encodes to that exact byte
        // (test_subprocess.test_undecodable_env).
        Object::WStr(cps) => {
            crate::stdlib::codecs_mod::encode_codepoints(cps, "utf-8", "surrogateescape").ok()
        }
        Object::Bytes(b) => Some(b.to_vec()),
        Object::ByteArray(b) => Some(b.borrow().clone()),
        _ => None,
    }
}

#[cfg(unix)]
fn obj_cstring(o: &Object, what: &str) -> Result<CString, RuntimeError> {
    let b = obj_bytes(o).ok_or_else(|| type_error(format!("{what}: expected str/bytes")))?;
    // CPython surfaces an interior NUL as `ValueError: embedded null byte`
    // (test_invalid_cmd / test_invalid_env assert on `ValueError`), not the
    // `TypeError` a naive conversion failure would imply.
    CString::new(b).map_err(|_| crate::error::value_error("embedded null byte"))
}

#[cfg(unix)]
fn opt_int(o: Option<&Object>) -> Option<i64> {
    match o {
        Some(Object::Int(n)) => Some(*n),
        Some(Object::Bool(b)) => Some(i64::from(*b)),
        _ => None,
    }
}

#[cfg(unix)]
fn truthy(o: Option<&Object>) -> bool {
    matches!(o, Some(Object::Bool(true)) | Some(Object::Int(1)))
}

/// Manual lowercase-hex of a small unsigned into `buf`, returning the
/// number of bytes written. Avoids `format!`'s allocator in the forked
/// child (async-signal-safety).
#[cfg(unix)]
fn write_hex(buf: &mut [u8], mut v: u32) -> usize {
    if v == 0 {
        if !buf.is_empty() {
            buf[0] = b'0';
        }
        return 1;
    }
    let mut tmp = [0u8; 8];
    let mut n = 0;
    while v > 0 && n < tmp.len() {
        let d = (v & 0xf) as u8;
        tmp[n] = if d < 10 { b'0' + d } else { b'a' + d - 10 };
        v >>= 4;
        n += 1;
    }
    let mut out = 0;
    while out < n && out < buf.len() {
        buf[out] = tmp[n - 1 - out];
        out += 1;
    }
    out
}

/// Report a child-side failure to the parent and `_exit`. Uses only a
/// stack buffer + `write`/`_exit` so it is async-signal-safe.
///
/// # Safety
/// Must run only in the forked child; `errpipe_write` must be a valid fd.
#[cfg(unix)]
unsafe fn child_report(errpipe_write: i32, exc: &[u8], errno_val: i32, msg: &[u8]) -> ! {
    let mut buf = [0u8; 256];
    let mut n = 0usize;
    for &b in exc {
        if n < buf.len() {
            buf[n] = b;
            n += 1;
        }
    }
    if n < buf.len() {
        buf[n] = b':';
        n += 1;
    }
    n += write_hex(&mut buf[n..], errno_val as u32);
    if n < buf.len() {
        buf[n] = b':';
        n += 1;
    }
    for &b in msg {
        if n < buf.len() {
            buf[n] = b;
            n += 1;
        }
    }
    unsafe {
        libc::write(errpipe_write, buf.as_ptr().cast(), n);
        libc::_exit(255)
    }
}

#[cfg(unix)]
fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// `fork_exec(args, executable_list, close_fds, fds_to_keep, cwd,
/// env_list, p2cread, p2cwrite, c2pread, c2pwrite, errread, errwrite,
/// errpipe_read, errpipe_write, restore_signals, call_setsid,
/// pgid_to_set, gid, extra_groups, uid, child_umask, preexec_fn,
/// allow_vfork)` — CPython 3.13's `_posixsubprocess.fork_exec`.
#[cfg(unix)]
fn fork_exec(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.len() < 23 {
        return Err(type_error(format!(
            "fork_exec expected 23 arguments, got {}",
            args.len()
        )));
    }
    let process_args = &args[0];
    let exec_list = &args[1];
    let close_fds = truthy(Some(&args[2]));
    let fds_to_keep = &args[3];
    let cwd = &args[4];
    let env_list = &args[5];
    let p2cread = opt_int(Some(&args[6])).unwrap_or(-1) as i32;
    let p2cwrite = opt_int(Some(&args[7])).unwrap_or(-1) as i32;
    let c2pread = opt_int(Some(&args[8])).unwrap_or(-1) as i32;
    let mut c2pwrite = opt_int(Some(&args[9])).unwrap_or(-1) as i32;
    let errread = opt_int(Some(&args[10])).unwrap_or(-1) as i32;
    let mut errwrite = opt_int(Some(&args[11])).unwrap_or(-1) as i32;
    let errpipe_read = opt_int(Some(&args[12])).unwrap_or(-1) as i32;
    let errpipe_write = opt_int(Some(&args[13])).unwrap_or(-1) as i32;
    let restore_signals = truthy(Some(&args[14]));
    let call_setsid = truthy(Some(&args[15]));
    let pgid_to_set = opt_int(Some(&args[16])).unwrap_or(-1);
    let gid = opt_int(Some(&args[17]));
    let extra_groups = &args[18];
    let uid = opt_int(Some(&args[19]));
    let child_umask = opt_int(Some(&args[20])).unwrap_or(-1);
    let preexec_fn = &args[21];

    // ---- Parent: build all C data BEFORE forking (no alloc in child). ----
    let exec_items = crate::stdlib::os::sequence_items(exec_list)
        .ok_or_else(|| type_error("fork_exec: executable_list must be a sequence"))?;
    let exec_paths: Vec<CString> = exec_items
        .iter()
        .map(|o| obj_cstring(o, "executable"))
        .collect::<Result<_, _>>()?;
    let exec_ptrs: Vec<*const libc::c_char> = exec_paths.iter().map(|c| c.as_ptr()).collect();

    let arg_items = crate::stdlib::os::sequence_items(process_args)
        .ok_or_else(|| type_error("fork_exec: args must be a sequence"))?;
    let argv_owned: Vec<CString> = arg_items
        .iter()
        .map(|o| obj_cstring(o, "arg"))
        .collect::<Result<_, _>>()?;
    let mut argv_ptrs: Vec<*const libc::c_char> = argv_owned.iter().map(|c| c.as_ptr()).collect();
    argv_ptrs.push(std::ptr::null());

    let envp_owned: Option<Vec<CString>> = match env_list {
        Object::None => None,
        other => {
            let items = crate::stdlib::os::sequence_items(other)
                .ok_or_else(|| type_error("fork_exec: env_list must be a sequence or None"))?;
            Some(
                items
                    .iter()
                    .map(|o| obj_cstring(o, "env"))
                    .collect::<Result<_, _>>()?,
            )
        }
    };
    let envp_ptrs: Option<Vec<*const libc::c_char>> = envp_owned.as_ref().map(|v| {
        let mut p: Vec<*const libc::c_char> = v.iter().map(|c| c.as_ptr()).collect();
        p.push(std::ptr::null());
        p
    });

    let cwd_c: Option<CString> = match cwd {
        Object::None => None,
        other => Some(obj_cstring(other, "cwd")?),
    };

    let keep: Vec<i32> = crate::stdlib::os::sequence_items(fds_to_keep)
        .ok_or_else(|| type_error("fork_exec: fds_to_keep must be a sequence"))?
        .iter()
        .filter_map(|o| opt_int(Some(o)).map(|n| n as i32))
        .collect();

    let groups: Vec<libc::gid_t> = match extra_groups {
        Object::None => Vec::new(),
        other => crate::stdlib::os::sequence_items(other)
            .unwrap_or_default()
            .iter()
            .filter_map(|o| opt_int(Some(o)).map(|n| n as libc::gid_t))
            .collect(),
    };

    let has_preexec = !matches!(preexec_fn, Object::None);

    // ---- fork ----
    // SAFETY: `fork(2)`. The child runs only the async-signal-safe sequence
    // below (the lone exception, `preexec_fn`, mirrors CPython's documented
    // "unsafe" contract) and always `exec`s or `_exit`s.
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err(crate::error::io_error_to_py(
            &std::io::Error::last_os_error(),
        ));
    }
    if pid > 0 {
        // Parent.
        return Ok(Object::Int(i64::from(pid)));
    }

    // ---- Child (pid == 0) ----
    unsafe {
        // Do NOT reset the signal mask here. CPython's `child_exec`
        // (`_posixsubprocess.c`) leaves the inherited mask untouched, and
        // `multiprocessing.resource_tracker._launch` relies on exactly that:
        // it `pthread_sigmask(SIG_BLOCK, {SIGINT, SIGTERM})` *right before*
        // forking so the tracker child inherits those blocked (bpo-33613).
        // That keeps a signal racing in during the child's startup *pending*
        // until `resource_tracker.main()` installs `SIG_IGN` and unblocks —
        // without it, an early `SIGINT`/`SIGTERM` (still `SIG_DFL` until the
        // VM arms its dispositions) kills the tracker, and the test's
        // `os.kill(tracker_pid, SIGINT)` (`test_resource_tracker_sigint`,
        // `should_die=False`) would then leak a REGISTER and surface a
        // spurious "resource_tracker: process died unexpectedly" warning.
        // The child inherits the forking (VM) thread's mask, which the VM
        // restored to the process's inherited mask at startup, so a plain
        // `subprocess.Popen` child still starts with the usual (empty) mask.

        // make_inheritable: clear CLOEXEC on every fd we promised to keep
        // (pass_fds + the std pipe ends) so they survive the coming exec.
        // errpipe_write is the exception — it stays CLOEXEC so the parent reads
        // EOF on a successful exec (test_pass_fds_inheritable).
        for &fd in &keep {
            if fd != errpipe_write {
                clear_cloexec(fd);
            }
        }

        // Close the parent's ends of the pipes inside the child so it can't
        // accidentally hold them open (and so the relocation dups below can
        // reclaim the freed low-numbered slots).
        if p2cwrite != -1 {
            libc::close(p2cwrite);
        }
        if c2pread != -1 {
            libc::close(c2pread);
        }
        if errread != -1 {
            libc::close(errread);
        }
        if errpipe_read != -1 {
            libc::close(errpipe_read);
        }

        // #12607 / issue32270: when a child fd we still need sits on 0/1/2, the
        // dup2() sequence below would clobber it before use. Relocate it above
        // the std range first (and mark the copy CLOEXEC so it doesn't leak).
        if c2pwrite == 0 {
            c2pwrite = libc::dup(c2pwrite);
            if c2pwrite < 0 {
                child_report(errpipe_write, b"OSError", errno(), b"");
            }
            set_cloexec(c2pwrite);
        }
        while errwrite == 0 || errwrite == 1 {
            errwrite = libc::dup(errwrite);
            if errwrite < 0 {
                child_report(errpipe_write, b"OSError", errno(), b"");
            }
            set_cloexec(errwrite);
        }

        // Dup the (possibly relocated) pipe ends onto 0/1/2. When a source is
        // already the target, dup2 is a no-op that would *keep* CLOEXEC, so we
        // clear it by hand (issue #10806).
        if p2cread == 0 {
            clear_cloexec(0);
        } else if p2cread != -1 && libc::dup2(p2cread, 0) == -1 {
            child_report(errpipe_write, b"OSError", errno(), b"");
        }
        if c2pwrite == 1 {
            clear_cloexec(1);
        } else if c2pwrite != -1 && libc::dup2(c2pwrite, 1) == -1 {
            child_report(errpipe_write, b"OSError", errno(), b"");
        }
        if errwrite == 2 {
            clear_cloexec(2);
        } else if errwrite != -1 && libc::dup2(errwrite, 2) == -1 {
            child_report(errpipe_write, b"OSError", errno(), b"");
        }

        // chdir(cwd) — report as the special "noexec:chdir" message.
        if let Some(c) = &cwd_c {
            if libc::chdir(c.as_ptr()) != 0 {
                child_report(errpipe_write, b"OSError", errno(), b"noexec:chdir");
            }
        }

        if child_umask >= 0 {
            libc::umask(child_umask as libc::mode_t);
        }

        if restore_signals {
            libc::signal(libc::SIGPIPE, libc::SIG_DFL);
            libc::signal(libc::SIGXFSZ, libc::SIG_DFL);
        }

        if call_setsid {
            libc::setsid();
        }
        if pgid_to_set >= 0 && libc::setpgid(0, pgid_to_set as libc::pid_t) != 0 {
            child_report(errpipe_write, b"OSError", errno(), b"noexec:setpgid");
        }

        if !groups.is_empty() {
            libc::setgroups(groups.len() as _, groups.as_ptr());
        }
        if let Some(g) = gid {
            if libc::setgid(g as libc::gid_t) != 0 {
                child_report(errpipe_write, b"OSError", errno(), b"noexec:setgid");
            }
        }
        if let Some(u) = uid {
            if libc::setuid(u as libc::uid_t) != 0 {
                child_report(errpipe_write, b"OSError", errno(), b"noexec:setuid");
            }
        }

        // preexec_fn (NOT async-signal-safe; matches CPython's contract).
        if has_preexec {
            if let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() {
                let interp = &mut *ptr;
                if interp.call_object(preexec_fn.clone(), &[], &[]).is_err() {
                    child_report(
                        errpipe_write,
                        b"SubprocessError",
                        0,
                        b"Exception occurred in preexec_fn.",
                    );
                }
            }
        }

        // Close inherited fds (>= 3) except fds_to_keep — *after* preexec_fn so
        // any descriptors it opened (e.g. a dup2 target) are swept too
        // (test_close_fds_after_preexec).
        if close_fds {
            close_open_fds(3, &keep);
        }

        // Exec loop over the candidate executables.
        let mut saved_errno = 0i32;
        for &path in &exec_ptrs {
            if let Some(env) = &envp_ptrs {
                libc::execve(path, argv_ptrs.as_ptr(), env.as_ptr());
            } else {
                libc::execv(path, argv_ptrs.as_ptr());
            }
            let e = errno();
            if e != libc::ENOENT && e != libc::ENOTDIR && saved_errno == 0 {
                saved_errno = e;
            }
        }
        let report_errno = if saved_errno != 0 {
            saved_errno
        } else {
            errno()
        };
        child_report(errpipe_write, b"OSError", report_errno, b"");
    }
}

/// Clear the close-on-exec flag on `fd` (async-signal-safe).
///
/// # Safety
/// `fd` must be a valid descriptor; called only in the forked child.
#[cfg(unix)]
unsafe fn clear_cloexec(fd: i32) {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags >= 0 {
        unsafe { libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) };
    }
}

/// Set the close-on-exec flag on `fd` (async-signal-safe). Used on the
/// relocation dups so the temporary high fds don't leak into the exec'd image.
///
/// # Safety
/// `fd` must be a valid descriptor; called only in the forked child.
#[cfg(unix)]
unsafe fn set_cloexec(fd: i32) {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags >= 0 {
        unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) };
    }
}

/// Close every open fd `>= start` except those in the `keep` set
/// (async-signal-safe; ignores `EBADF`).
///
/// Prefers enumerating the per-process fd directory (`/proc/self/fd` on
/// Linux, `/dev/fd` elsewhere), exactly as CPython's `_close_open_fds`
/// does. That matters because a bounded `[start, sysconf(_SC_OPEN_MAX))`
/// sweep misses descriptors inherited *above* a lowered `RLIMIT_NOFILE`
/// (`sysconf`/`getrlimit` both report only the lowered soft limit, while
/// the original `rlim_max` may be effectively unbounded and impossible to
/// brute-force) — see `test_close_fds_when_max_fd_is_lowered` (bpo-21618).
/// Falls back to the bounded sweep when the directory can't be read.
///
/// # Safety
/// Called only in the forked child.
#[cfg(unix)]
unsafe fn close_open_fds(start: i32, keep: &[i32]) {
    if unsafe { close_open_fds_via_dir(start, keep) } {
        return;
    }
    unsafe { close_open_fds_brute_force(start, keep) };
}

/// Parse a NUL-terminated ASCII fd-number directory entry name (e.g.
/// `b"42\0"`). Returns `None` for `.`/`..`/non-numeric/overflowing names.
///
/// # Safety
/// `p` must point to a NUL-terminated C string.
#[cfg(unix)]
unsafe fn parse_fd_name(mut p: *const libc::c_char) -> Option<i32> {
    let mut val: i64 = 0;
    let mut any = false;
    loop {
        let c = unsafe { *p } as u8;
        if c == 0 {
            break;
        }
        if !c.is_ascii_digit() {
            return None;
        }
        val = val * 10 + i64::from(c - b'0');
        if val > i64::from(i32::MAX) {
            return None;
        }
        any = true;
        p = unsafe { p.add(1) };
    }
    if any {
        Some(val as i32)
    } else {
        None
    }
}

/// Drain the collected descriptors, falling back to the bounded sweep when
/// more were found than the fixed scratch buffer could hold.
///
/// # Safety
/// Called only in the forked child.
#[cfg(unix)]
unsafe fn drain_close(to_close: &[i32], overflow: bool, start: i32, keep: &[i32]) {
    for &fd in to_close {
        unsafe { libc::close(fd) };
    }
    if overflow {
        unsafe { close_open_fds_brute_force(start, keep) };
    }
}

/// Enumerate `/proc/self/fd` via the raw `getdents64` syscall (no malloc,
/// so it's safe between `fork` and `exec` even if other threads existed)
/// and close every descriptor `>= start` not in `keep`. Returns `true` on
/// success, `false` if the directory couldn't be opened.
///
/// # Safety
/// Called only in the forked child.
#[cfg(all(unix, target_os = "linux"))]
unsafe fn close_open_fds_via_dir(start: i32, keep: &[i32]) -> bool {
    const FD_DIR: &[u8] = b"/proc/self/fd\0";
    // `d_reclen` (u16) is at byte offset 16 in `linux_dirent64`; the name
    // follows the header at offset 19. Stable kernel ABI.
    const RECLEN_OFF: usize = 16;
    const NAME_OFF: usize = 19;

    let dir_fd = unsafe {
        libc::open(
            FD_DIR.as_ptr().cast::<libc::c_char>(),
            libc::O_RDONLY | libc::O_CLOEXEC,
        )
    };
    if dir_fd < 0 {
        return false;
    }

    // 8-aligned scratch so the per-entry header reads are well-aligned.
    #[repr(align(8))]
    struct Buf([u8; 8192]);
    let mut buf = Buf([0u8; 8192]);

    // Collect first, close after the directory is fully read, so closing a
    // listed descriptor can't perturb the kernel's directory iteration.
    let mut to_close = [0i32; 4096];
    let mut count = 0usize;
    let mut overflow = false;

    loop {
        let nread = unsafe {
            libc::syscall(
                libc::SYS_getdents64,
                dir_fd,
                buf.0.as_mut_ptr(),
                buf.0.len(),
            ) as isize
        };
        if nread <= 0 {
            break; // EOF or error
        }
        let mut off: isize = 0;
        while off < nread {
            let ent = unsafe { buf.0.as_ptr().offset(off) };
            // `d_reclen` is a native-endian u16; read it byte-wise to avoid a
            // potentially-unaligned u16 pointer load.
            let reclen_lo = unsafe { *ent.add(RECLEN_OFF) };
            let reclen_hi = unsafe { *ent.add(RECLEN_OFF + 1) };
            // `isize: From<u16>` is not provided (isize may be 16-bit), so
            // widen with `as` — a `u16` always fits the `isize` of every target
            // WeavePy builds for.
            let reclen = u16::from_ne_bytes([reclen_lo, reclen_hi]) as isize;
            if reclen <= 0 {
                break;
            }
            let name_ptr = unsafe { ent.add(NAME_OFF).cast::<libc::c_char>() };
            if let Some(fd) = unsafe { parse_fd_name(name_ptr) } {
                if fd >= start && fd != dir_fd && !keep.contains(&fd) {
                    if count < to_close.len() {
                        to_close[count] = fd;
                        count += 1;
                    } else {
                        overflow = true;
                    }
                }
            }
            off += reclen;
        }
    }

    unsafe { libc::close(dir_fd) };
    unsafe { drain_close(&to_close[..count], overflow, start, keep) };
    true
}

/// Enumerate `/dev/fd` via `opendir`/`readdir` and close every descriptor
/// `>= start` not in `keep`. Returns `true` on success, `false` if the
/// directory couldn't be opened. This mirrors CPython's non-Linux
/// `_close_open_fds_maybe_unsafe` path (the libc directory walker is the
/// only portable option there).
///
/// # Safety
/// Called only in the forked child.
#[cfg(all(unix, not(target_os = "linux")))]
unsafe fn close_open_fds_via_dir(start: i32, keep: &[i32]) -> bool {
    const FD_DIR: &[u8] = b"/dev/fd\0";
    let dir = unsafe { libc::opendir(FD_DIR.as_ptr().cast::<libc::c_char>()) };
    if dir.is_null() {
        return false;
    }
    let dir_fd = unsafe { libc::dirfd(dir) };

    // Collect first, close after `closedir`, so closing a listed descriptor
    // can't perturb the directory iteration.
    let mut to_close = [0i32; 4096];
    let mut count = 0usize;
    let mut overflow = false;

    loop {
        let ent = unsafe { libc::readdir(dir) };
        if ent.is_null() {
            break;
        }
        let name_ptr = unsafe { (*ent).d_name.as_ptr() };
        if let Some(fd) = unsafe { parse_fd_name(name_ptr) } {
            if fd >= start && fd != dir_fd && !keep.contains(&fd) {
                if count < to_close.len() {
                    to_close[count] = fd;
                    count += 1;
                } else {
                    overflow = true;
                }
            }
        }
    }

    unsafe { libc::closedir(dir) };
    unsafe { drain_close(&to_close[..count], overflow, start, keep) };
    true
}

/// Close every fd in `[start, max)` except those in `keep` — the bounded
/// fallback when the fd directory is unavailable (async-signal-safe;
/// ignores `EBADF`).
///
/// # Safety
/// Called only in the forked child.
#[cfg(unix)]
unsafe fn close_open_fds_brute_force(start: i32, keep: &[i32]) {
    let max = {
        let m = unsafe { libc::sysconf(libc::_SC_OPEN_MAX) };
        if m <= 0 || m > 1 << 20 {
            4096
        } else {
            m as i32
        }
    };
    let mut fd = start;
    while fd < max {
        if !keep.contains(&fd) {
            unsafe { libc::close(fd) };
        }
        fd += 1;
    }
}

#[cfg(not(unix))]
fn fork_exec(_args: &[Object]) -> Result<Object, RuntimeError> {
    Err(crate::error::not_implemented_error(
        "_posixsubprocess.fork_exec requires POSIX",
    ))
}
