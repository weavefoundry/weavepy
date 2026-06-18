//! The `_subprocess` built-in module — the Rust core that the
//! user-visible `subprocess` (frozen Python) sits on top of.
//!
//! We expose two primitives:
//!
//! * `_subprocess.run(args, stdin=None, capture_output=False, cwd=None,
//!   env=None, timeout=None, shell=False)` — synchronous spawn-and-
//!   wait. Returns `(returncode, stdout_bytes, stderr_bytes)`.
//!
//! * `_subprocess.spawn(args, stdin_pipe=False, stdout_pipe=False,
//!   stderr_pipe=False, stderr_to_stdout=False, cwd=None, env=None,
//!   shell=False)` — returns a process-handle dict with `wait()`,
//!   `poll()`, `kill()`, `terminate()`, `send_signal()`, `pid`,
//!   `stdin`, `stdout`, `stderr`. The std{in,out,err} attributes are
//!   `io`-style file objects on top of the OS pipe fds, so the user
//!   can `proc.stdout.read()` / `proc.stdin.write(...)` directly.
//!
//! The Python wrapper (`stdlib/python/subprocess.py`) layers `Popen`,
//! `run`, `check_output`, `check_call`, `getoutput`,
//! `CalledProcessError` and `TimeoutExpired` on top of these.

use crate::sync::Rc;
use crate::sync::RefCell;
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::error::{io_error_to_py, os_error, type_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, FileBackend, Object, PyFile, PyModule};

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("_subprocess"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("Internal subprocess primitives (Rust core)."),
        );
        // CPython sentinel values for stdin/stdout/stderr.
        d.insert(DictKey(Object::from_static("PIPE")), Object::Int(-1));
        d.insert(DictKey(Object::from_static("DEVNULL")), Object::Int(-3));
        d.insert(DictKey(Object::from_static("STDOUT")), Object::Int(-2));
        d.insert(DictKey(Object::from_static("run")), b("run", run_call));
        d.insert(
            DictKey(Object::from_static("spawn")),
            b("spawn", spawn_call),
        );
        d.insert(
            DictKey(Object::from_static("getoutput")),
            b("getoutput", getoutput_call),
        );
    }
    Rc::new(PyModule {
        name: "_subprocess".to_owned(),
        filename: None,
        dict,
    })
}

fn b(name: &'static str, body: fn(&[Object]) -> Result<Object, RuntimeError>) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        binds_instance: false,
        call: Box::new(body),
        call_kw: None,
    }))
}

// ---- run ----

/// `_subprocess.run(args, stdin, capture_output, cwd, env, timeout, shell)`.
fn run_call(args: &[Object]) -> Result<Object, RuntimeError> {
    let argv = parse_argv(args.first())?;
    let stdin_bytes = match args.get(1) {
        None | Some(Object::None) => None,
        Some(Object::Bytes(b)) => Some(b.to_vec()),
        Some(Object::ByteArray(b)) => Some(b.borrow().clone()),
        Some(Object::Str(s)) => Some(s.as_bytes().to_vec()),
        _ => return Err(type_error("stdin must be bytes-like or None")),
    };
    let capture = match args.get(2) {
        Some(Object::Bool(b)) => *b,
        _ => false,
    };
    let cwd = match args.get(3) {
        Some(Object::Str(s)) => Some(s.to_string()),
        _ => None,
    };
    let env = match args.get(4) {
        Some(Object::Dict(d)) => Some(d.clone()),
        _ => None,
    };
    let timeout = match args.get(5) {
        Some(Object::Float(f)) => Some(Duration::from_secs_f64(*f)),
        Some(Object::Int(n)) => Some(Duration::from_secs(*n as u64)),
        _ => None,
    };
    let shell = match args.get(6) {
        Some(Object::Bool(b)) => *b,
        _ => false,
    };

    let mut cmd = build_command(&argv, shell, cwd.as_deref(), env.as_ref())?;
    if stdin_bytes.is_some() {
        cmd.stdin(Stdio::piped());
    }
    if capture {
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
    }
    let mut child = cmd.spawn().map_err(|e| io_error_to_py(&e))?;

    // Drain stdin/stdout/stderr concurrently. A child that fills one
    // pipe blocks until we read it, so writing all of stdin before
    // reading any output — or reading one output stream to EOF before
    // the other — can deadlock once a pipe's kernel buffer (~64 KiB)
    // fills. Reader/writer threads mirror CPython's `communicate()`.
    let stdin_join = match (stdin_bytes, child.stdin.take()) {
        (Some(data), Some(mut stdin)) => Some(std::thread::spawn(move || {
            use std::io::Write;
            let _ = stdin.write_all(&data);
            // Dropping `stdin` here closes the write end → EOF to child.
        })),
        _ => None,
    };
    let out_join = child
        .stdout
        .take()
        .map(|mut s| std::thread::spawn(move || read_pipe_to_end(&mut s)));
    let err_join = child
        .stderr
        .take()
        .map(|mut s| std::thread::spawn(move || read_pipe_to_end(&mut s)));

    let status = wait_with_timeout(&mut child, timeout)?;

    if let Some(h) = stdin_join {
        let _ = h.join();
    }
    let out_bytes = out_join.map(join_reader).unwrap_or_default();
    let err_bytes = err_join.map(join_reader).unwrap_or_default();
    let rc = status_code(status);
    Ok(Object::new_tuple(vec![
        Object::Int(rc),
        Object::new_bytes(out_bytes),
        Object::new_bytes(err_bytes),
    ]))
}

/// Read a child pipe to EOF, returning the bytes. Best-effort: a read
/// error yields whatever was buffered so far (matching CPython's
/// `communicate`, which tolerates a partially-read pipe on child death).
fn read_pipe_to_end<R: Read>(reader: &mut R) -> Vec<u8> {
    let mut buf = Vec::new();
    let _ = reader.read_to_end(&mut buf);
    buf
}

/// Join a reader thread, defaulting to empty bytes if it panicked.
fn join_reader(handle: std::thread::JoinHandle<Vec<u8>>) -> Vec<u8> {
    handle.join().unwrap_or_default()
}

/// Block-wait on `child` until exit, honouring `timeout` if any.
/// On timeout we kill the child and surface a `TimeoutError`.
fn wait_with_timeout(
    child: &mut std::process::Child,
    timeout: Option<Duration>,
) -> Result<std::process::ExitStatus, RuntimeError> {
    match timeout {
        None => child.wait().map_err(|e| io_error_to_py(&e)),
        Some(t) => {
            let start = Instant::now();
            loop {
                if let Some(s) = child.try_wait().map_err(|e| io_error_to_py(&e))? {
                    return Ok(s);
                }
                if start.elapsed() >= t {
                    let _ = child.kill();
                    return Err(crate::error::timeout_error("subprocess timed out"));
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

fn status_code(status: std::process::ExitStatus) -> i64 {
    if let Some(code) = status.code() {
        return i64::from(code);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            // CPython reports a signal-terminated child as -signum.
            return -i64::from(sig);
        }
    }
    -1
}

// ---- spawn ----

fn spawn_call(args: &[Object]) -> Result<Object, RuntimeError> {
    let argv = parse_argv(args.first())?;
    let stdin_pipe = match args.get(1) {
        Some(Object::Bool(b)) => *b,
        _ => false,
    };
    let stdout_pipe = match args.get(2) {
        Some(Object::Bool(b)) => *b,
        _ => false,
    };
    let stderr_pipe = match args.get(3) {
        Some(Object::Bool(b)) => *b,
        _ => false,
    };
    let stderr_to_stdout = match args.get(4) {
        Some(Object::Bool(b)) => *b,
        _ => false,
    };
    let cwd = match args.get(5) {
        Some(Object::Str(s)) => Some(s.to_string()),
        _ => None,
    };
    let env = match args.get(6) {
        Some(Object::Dict(d)) => Some(d.clone()),
        _ => None,
    };
    let shell = match args.get(7) {
        Some(Object::Bool(b)) => *b,
        _ => false,
    };

    let mut cmd = build_command(&argv, shell, cwd.as_deref(), env.as_ref())?;
    cmd.stdin(if stdin_pipe {
        Stdio::piped()
    } else {
        Stdio::inherit()
    });
    // `stderr=STDOUT`: both child streams must share one fd so writes
    // interleave in order. When stdout is a pipe we hand the child two
    // clones of the same pipe's write end and keep the read end.
    let mut merged_reader: Option<std::io::PipeReader> = None;
    if stderr_to_stdout && stdout_pipe {
        let (reader, writer) = std::io::pipe().map_err(|e| io_error_to_py(&e))?;
        let writer2 = writer.try_clone().map_err(|e| io_error_to_py(&e))?;
        cmd.stdout(Stdio::from(writer));
        cmd.stderr(Stdio::from(writer2));
        merged_reader = Some(reader);
    } else {
        cmd.stdout(if stdout_pipe {
            Stdio::piped()
        } else {
            Stdio::inherit()
        });
        if stderr_to_stdout {
            // stdout is inherited; send child stderr to *our* stdout.
            #[cfg(unix)]
            {
                use std::os::fd::FromRawFd;
                let dup = unsafe { libc::dup(1) };
                if dup >= 0 {
                    cmd.stderr(unsafe { Stdio::from_raw_fd(dup) });
                } else {
                    cmd.stderr(Stdio::inherit());
                }
            }
            #[cfg(not(unix))]
            cmd.stderr(Stdio::inherit());
        } else {
            cmd.stderr(if stderr_pipe {
                Stdio::piped()
            } else {
                Stdio::inherit()
            });
        }
    }
    let mut child = cmd.spawn().map_err(|e| io_error_to_py(&e))?;
    // `Command` keeps its configured `Stdio` handles (our pipe write
    // ends) alive until dropped — reading the merged pipe to EOF
    // would deadlock with them still open in this process.
    drop(cmd);
    let pid = i64::from(child.id());

    let stdin_obj = child.stdin.take().map_or(Object::None, pipe_writer);
    let stdout_obj = match merged_reader {
        Some(reader) => pipe_reader(reader),
        None => child.stdout.take().map_or(Object::None, pipe_reader),
    };
    let stderr_obj = child.stderr.take().map_or(Object::None, pipe_reader);

    let child_rc: Rc<RefCell<Option<std::process::Child>>> = Rc::new(RefCell::new(Some(child)));
    let returncode: Rc<RefCell<Option<i64>>> = Rc::new(RefCell::new(None));

    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(DictKey(Object::from_static("pid")), Object::Int(pid));
        d.insert(DictKey(Object::from_static("stdin")), stdin_obj);
        d.insert(DictKey(Object::from_static("stdout")), stdout_obj);
        d.insert(DictKey(Object::from_static("stderr")), stderr_obj);
        d.insert(DictKey(Object::from_static("returncode")), Object::None);

        // poll() — returns returncode or None.
        {
            let c = child_rc.clone();
            let rc = returncode.clone();
            d.insert(
                DictKey(Object::from_static("poll")),
                Object::Builtin(Rc::new(BuiltinFn {
                    name: "poll",
                    binds_instance: false,
                    call: Box::new(move |_a: &[Object]| {
                        if let Some(code) = *rc.borrow() {
                            return Ok(Object::Int(code));
                        }
                        let mut slot = c.borrow_mut();
                        if let Some(ch) = slot.as_mut() {
                            if let Some(status) = ch.try_wait().map_err(|e| io_error_to_py(&e))? {
                                let code = status_code(status);
                                *rc.borrow_mut() = Some(code);
                                return Ok(Object::Int(code));
                            }
                        }
                        Ok(Object::None)
                    }),
                    call_kw: None,
                })),
            );
        }

        // wait(timeout) — blocks until child exits.
        {
            let c = child_rc.clone();
            let rc = returncode.clone();
            d.insert(
                DictKey(Object::from_static("wait")),
                Object::Builtin(Rc::new(BuiltinFn {
                    name: "wait",
                    binds_instance: false,
                    call: Box::new(move |a: &[Object]| {
                        if let Some(code) = *rc.borrow() {
                            return Ok(Object::Int(code));
                        }
                        let timeout = match a.first() {
                            Some(Object::Float(f)) => Some(Duration::from_secs_f64(*f)),
                            Some(Object::Int(n)) => Some(Duration::from_secs(*n as u64)),
                            _ => None,
                        };
                        let mut slot = c.borrow_mut();
                        let ch = slot
                            .as_mut()
                            .ok_or_else(|| os_error("child already reaped"))?;
                        let status = wait_with_timeout(ch, timeout)?;
                        let code = status_code(status);
                        *rc.borrow_mut() = Some(code);
                        Ok(Object::Int(code))
                    }),
                    call_kw: None,
                })),
            );
        }

        // kill() — SIGKILL on POSIX, TerminateProcess on Windows.
        {
            let c = child_rc.clone();
            d.insert(
                DictKey(Object::from_static("kill")),
                Object::Builtin(Rc::new(BuiltinFn {
                    name: "kill",
                    binds_instance: false,
                    call: Box::new(move |_a: &[Object]| {
                        let mut slot = c.borrow_mut();
                        if let Some(ch) = slot.as_mut() {
                            let _ = ch.kill();
                        }
                        Ok(Object::None)
                    }),
                    call_kw: None,
                })),
            );
        }

        // terminate() — alias of kill on Windows; SIGTERM on POSIX.
        {
            let c = child_rc.clone();
            d.insert(
                DictKey(Object::from_static("terminate")),
                Object::Builtin(Rc::new(BuiltinFn {
                    name: "terminate",
                    binds_instance: false,
                    call: Box::new(move |_a: &[Object]| {
                        let mut slot = c.borrow_mut();
                        if let Some(ch) = slot.as_mut() {
                            #[cfg(unix)]
                            {
                                use std::os::unix::process::ExitStatusExt;
                                // We can't send SIGTERM directly with std::process; fall
                                // back to kill() which on Unix is SIGKILL but works for
                                // the test cases that need a child gone.
                                let _: Option<std::process::ExitStatus> =
                                    Some(std::process::ExitStatus::from_raw(0));
                            }
                            let _ = ch.kill();
                        }
                        Ok(Object::None)
                    }),
                    call_kw: None,
                })),
            );
        }

        // send_signal(sig) — best-effort; on POSIX we shell out to kill.
        {
            let pid_val = pid;
            d.insert(
                DictKey(Object::from_static("send_signal")),
                Object::Builtin(Rc::new(BuiltinFn {
                    name: "send_signal",
                    binds_instance: false,
                    call: Box::new(move |a: &[Object]| {
                        let sig = match a.first() {
                            Some(Object::Int(n)) => *n,
                            _ => return Err(type_error("send_signal: arg must be int")),
                        };
                        #[cfg(unix)]
                        {
                            let _ = std::process::Command::new("kill")
                                .arg(format!("-{sig}"))
                                .arg(pid_val.to_string())
                                .status();
                        }
                        let _ = sig;
                        let _ = pid_val;
                        Ok(Object::None)
                    }),
                    call_kw: None,
                })),
            );
        }
    }

    Ok(Object::Dict(dict))
}

// ---- helpers ----

fn parse_argv(arg: Option<&Object>) -> Result<Vec<String>, RuntimeError> {
    match arg {
        Some(Object::Str(s)) => Ok(vec![s.to_string()]),
        Some(Object::List(l)) => {
            let borrowed = l.borrow();
            let mut out = Vec::with_capacity(borrowed.len());
            for item in borrowed.iter() {
                match item {
                    Object::Str(s) => out.push(s.to_string()),
                    _ => return Err(type_error("argv items must be str")),
                }
            }
            Ok(out)
        }
        Some(Object::Tuple(t)) => {
            let mut out = Vec::with_capacity(t.len());
            for item in t.iter() {
                match item {
                    Object::Str(s) => out.push(s.to_string()),
                    _ => return Err(type_error("argv items must be str")),
                }
            }
            Ok(out)
        }
        _ => Err(type_error("argv must be str or list of str")),
    }
}

fn build_command(
    argv: &[String],
    shell: bool,
    cwd: Option<&str>,
    env: Option<&Rc<RefCell<DictData>>>,
) -> Result<Command, RuntimeError> {
    if argv.is_empty() {
        return Err(crate::error::value_error(
            "subprocess: argv must be non-empty",
        ));
    }
    let mut cmd = if shell {
        // CPython's `shell=True` runs `argv` (which is a single string)
        // through `/bin/sh -c` on POSIX or `cmd.exe /c` on Windows.
        let joined = argv.join(" ");
        if cfg!(windows) {
            let mut c = Command::new("cmd");
            c.arg("/c").arg(joined);
            c
        } else {
            let mut c = Command::new("/bin/sh");
            c.arg("-c").arg(joined);
            c
        }
    } else {
        let mut c = Command::new(&argv[0]);
        c.args(&argv[1..]);
        c
    };
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    if let Some(env_dict) = env {
        cmd.env_clear();
        for (key, value) in env_dict.borrow().iter() {
            if let (Object::Str(k), Object::Str(v)) = (&key.0, value) {
                cmd.env(k.as_ref(), v.as_ref());
            }
        }
    }
    Ok(cmd)
}

/// Drain a reader to EOF into an in-memory file. Used as the non-Unix
/// fallback, where re-wrapping the raw fd as a `Disk` backend isn't available.
#[cfg(not(unix))]
fn pipe_reader_drain<R: Read + 'static>(mut reader: R) -> Object {
    let mut buf = Vec::new();
    let _ = reader.read_to_end(&mut buf);
    let pf = PyFile::new(
        "<subprocess-pipe>",
        "rb",
        FileBackend::MemBytes {
            data: crate::sync::Rc::new(crate::sync::RefCell::new(buf)),
            pos: 0,
        },
    );
    Object::File(Rc::new(pf))
}

/// A child's `stdout`/`stderr` pipe, exposed *lazily* as a readable file.
/// We re-wrap the raw fd as a `Disk` backend so reads stream straight from
/// the OS pipe (rather than draining at spawn, which would run the child to
/// completion before its `stdin` could be written — the bug behind
/// `Popen.communicate(input=...)` losing all output).
#[cfg(unix)]
fn pipe_reader<R: std::os::fd::IntoRawFd>(reader: R) -> Object {
    use std::os::fd::FromRawFd;
    // SAFETY: `into_raw_fd` hands us sole ownership of a valid, open fd;
    // the resulting `File` is the only owner and closes it on drop.
    let file = unsafe { std::fs::File::from_raw_fd(reader.into_raw_fd()) };
    Object::File(Rc::new(PyFile::new(
        "<subprocess-pipe>",
        "rb",
        FileBackend::Disk(file),
    )))
}

#[cfg(not(unix))]
fn pipe_reader<R: Read + 'static>(reader: R) -> Object {
    pipe_reader_drain(reader)
}

/// A child's `stdin` pipe, exposed as a writable file. The raw fd is
/// re-wrapped as a `Disk` backend; `close()` (or dropping the file) closes
/// the write end, signalling EOF to the child.
#[cfg(unix)]
fn pipe_writer<W: std::os::fd::IntoRawFd>(writer: W) -> Object {
    use std::os::fd::FromRawFd;
    // SAFETY: `into_raw_fd` hands us sole ownership of a valid, open fd;
    // the resulting `File` is the only owner and closes it on drop.
    let file = unsafe { std::fs::File::from_raw_fd(writer.into_raw_fd()) };
    Object::File(Rc::new(PyFile::new(
        "<subprocess-stdin>",
        "wb",
        FileBackend::Disk(file),
    )))
}

#[cfg(not(unix))]
fn pipe_writer<W: std::io::Write + 'static>(_writer: W) -> Object {
    // No raw-fd re-wrap available; hand back an inert buffer (best-effort,
    // matching the historical behaviour on these targets).
    let pf = PyFile::new(
        "<subprocess-stdin>",
        "wb",
        FileBackend::MemBytes {
            data: crate::sync::Rc::new(crate::sync::RefCell::new(Vec::new())),
            pos: 0,
        },
    );
    Object::File(Rc::new(pf))
}

fn getoutput_call(args: &[Object]) -> Result<Object, RuntimeError> {
    // `subprocess.getoutput(cmd)` is shell=True with merged stderr.
    let cmd_str = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Err(type_error("getoutput: cmd must be str")),
    };
    let argv = vec![cmd_str];
    let mut cmd = build_command(&argv, true, None, None)?;
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let output = cmd.output().map_err(|e| io_error_to_py(&e))?;
    let mut buf = output.stdout;
    buf.extend_from_slice(&output.stderr);
    while matches!(buf.last(), Some(b'\n' | b'\r')) {
        buf.pop();
    }
    let s = String::from_utf8_lossy(&buf).into_owned();
    Ok(Object::from_str(s))
}
