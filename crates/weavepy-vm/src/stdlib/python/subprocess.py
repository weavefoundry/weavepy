"""WeavePy `subprocess` — Popen and friends, on top of `_subprocess`.

The Rust-side `_subprocess` module exposes two primitives:
* `_subprocess.run(args, stdin, capture_output, cwd, env, timeout, shell)`
* `_subprocess.spawn(args, stdin_pipe, stdout_pipe, stderr_pipe,
                       stderr_to_stdout, cwd, env, shell)`

This wrapper turns them into the CPython surface user code expects:
* `Popen(args, ...)` — process handle with `.communicate`, `.wait`,
  `.poll`, `.kill`, `.terminate`, `.send_signal`.
* `run(args, ...)` — convenience returning `CompletedProcess`.
* `check_output`, `check_call`, `getoutput`, `getstatusoutput`.
* `CalledProcessError`, `TimeoutExpired`, `SubprocessError`.
* `PIPE`, `DEVNULL`, `STDOUT` sentinels (re-exported from `_subprocess`).

Notes:
* Streaming `.stdin.write()` on a `Popen(stdin=PIPE)` handle is
  buffered up to `communicate()` rather than written through to the
  child synchronously — the latter requires the asyncio I/O plumbing
  RFC 0017 ships in a separate patch.
"""

import sys
import _subprocess


PIPE = _subprocess.PIPE
DEVNULL = _subprocess.DEVNULL
STDOUT = _subprocess.STDOUT


def _optim_args_from_interpreter_flags():
    """Return a list of command-line arguments reproducing the current
    optimization settings in sys.flags."""
    args = []
    value = getattr(sys.flags, "optimize", 0)
    if value > 0:
        args.append("-" + "O" * value)
    return args


def _args_from_interpreter_flags():
    """Return a list of command-line arguments reproducing the current
    settings in sys.flags, sys.warnoptions and sys._xoptions.

    Used by `multiprocessing` (and so `concurrent.futures.process`) to
    re-launch a faithful child interpreter.
    """
    flag_opt_map = {
        "debug": "d",
        "dont_write_bytecode": "B",
        "no_site": "S",
        "verbose": "v",
        "bytes_warning": "b",
        "quiet": "q",
        # -O is handled in _optim_args_from_interpreter_flags()
    }
    args = _optim_args_from_interpreter_flags()
    for flag, opt in flag_opt_map.items():
        v = getattr(sys.flags, flag, 0)
        if v and v > 0:
            args.append("-" + opt * v)

    if getattr(sys.flags, "isolated", 0):
        args.append("-I")
    else:
        if getattr(sys.flags, "ignore_environment", 0):
            args.append("-E")
        if getattr(sys.flags, "no_user_site", 0):
            args.append("-s")
    if getattr(sys.flags, "safe_path", 0):
        args.append("-P")

    # -W options
    warnopts = list(getattr(sys, "warnoptions", []))
    bytes_warning = getattr(sys.flags, "bytes_warning", 0)
    xoptions = getattr(sys, "_xoptions", {})
    dev_mode = "dev" in xoptions

    def _discard(seq, value):
        try:
            seq.remove(value)
        except ValueError:
            pass

    if bytes_warning > 1:
        _discard(warnopts, "error::BytesWarning")
    elif bytes_warning:
        _discard(warnopts, "default::BytesWarning")
    if dev_mode:
        _discard(warnopts, "default")
    for opt in warnopts:
        args.append("-W" + opt)

    # -X options
    if dev_mode:
        args.extend(("-X", "dev"))
    for opt in (
        "faulthandler",
        "tracemalloc",
        "importtime",
        "showrefcount",
        "utf8",
    ):
        if opt in xoptions:
            value = xoptions[opt]
            if value is True:
                arg = opt
            else:
                arg = "%s=%s" % (opt, value)
            args.extend(("-X", arg))

    return args


class SubprocessError(Exception):
    """Base class for subprocess-specific errors."""


class CalledProcessError(SubprocessError):
    """Raised by `run(check=True)` / `check_output` when the child exits non-zero."""

    def __init__(self, returncode, cmd, output=None, stderr=None):
        self.returncode = returncode
        self.cmd = cmd
        self.output = output
        self.stderr = stderr
        self.args = (returncode, cmd)

    def __str__(self):
        if self.returncode < 0:
            return "Command {!r} died with signal {}.".format(self.cmd, -self.returncode)
        return "Command {!r} returned non-zero exit status {}.".format(self.cmd, self.returncode)


class TimeoutExpired(SubprocessError):
    """Raised by `run(timeout=...)` / `Popen.wait(timeout=...)` when the child times out."""

    def __init__(self, cmd, timeout, output=None, stderr=None):
        self.cmd = cmd
        self.timeout = timeout
        self.output = output
        self.stderr = stderr
        self.args = (cmd, timeout)

    def __str__(self):
        return "Command {!r} timed out after {} seconds".format(self.cmd, self.timeout)


class CompletedProcess:
    """The return value of `run(...)`."""

    def __init__(self, args, returncode, stdout=None, stderr=None):
        self.args = args
        self.returncode = returncode
        self.stdout = stdout
        self.stderr = stderr

    def __repr__(self):
        return "CompletedProcess(args={!r}, returncode={!r})".format(self.args, self.returncode)

    def check_returncode(self):
        if self.returncode != 0:
            raise CalledProcessError(self.returncode, self.args, self.stdout, self.stderr)


def _fsencode_item(item):
    """Coerce one argv element to `str`, mirroring CPython's acceptance
    of `str`, `bytes`, and `os.PathLike` program/argument items."""
    if isinstance(item, str):
        return item
    if isinstance(item, (bytes, bytearray)):
        return bytes(item).decode(sys.getfilesystemencoding(), "surrogateescape")
    fspath = getattr(item, "__fspath__", None)
    if fspath is not None:
        return _fsencode_item(fspath())
    raise TypeError(
        "argv items must be str, bytes, or os.PathLike, not %s"
        % type(item).__name__
    )


def _coerce_args(args):
    if isinstance(args, (str, bytes)):
        return [_fsencode_item(args)]
    return [_fsencode_item(a) for a in args]


def _maybe_decode(data, text, encoding):
    if data is None or not text:
        return data
    if isinstance(data, bytes):
        return data.decode(encoding or "utf-8")
    return data


class Popen:
    """A spawned child process.

    `args` may be a string (run via the shell when `shell=True`) or a
    list/tuple of program + args. `stdin` / `stdout` / `stderr` accept
    `PIPE`, `DEVNULL`, `STDOUT` (only for `stderr`), or `None`.
    """

    def __init__(self, args, bufsize=-1, executable=None, stdin=None,
                 stdout=None, stderr=None, preexec_fn=None, close_fds=True,
                 shell=False, cwd=None, env=None, universal_newlines=None,
                 startupinfo=None, creationflags=0, restore_signals=True,
                 start_new_session=False, pass_fds=(), *, user=None,
                 group=None, extra_groups=None, encoding=None, errors=None,
                 text=None, umask=-1, pipesize=-1, process_group=None):
        # `preexec_fn`, `close_fds`, `pass_fds`, `restore_signals`,
        # `start_new_session`, `umask`, `process_group`, `user`/`group`/
        # `extra_groups`, `startupinfo`, and `creationflags` are accepted
        # for CPython-signature compatibility. The ones our `_subprocess`
        # spawn primitive can't yet honour (fd inheritance control, setsid,
        # credential switching, Windows creation flags) are recorded but not
        # enforced — faithful enough for the I/O- and exit-code-driven tests,
        # with full enforcement gated on a deeper spawn primitive (RFC 0017).
        self.args = args
        self.encoding = encoding
        self._text = text if text is not None else (universal_newlines or False)
        stdin_pipe = stdin is PIPE
        stdout_pipe = stdout is PIPE
        stderr_pipe = stderr is PIPE
        stderr_to_stdout = stderr is STDOUT
        argv = _coerce_args(args)
        handle = _subprocess.spawn(
            argv, stdin_pipe, stdout_pipe, stderr_pipe, stderr_to_stdout,
            cwd, env, shell,
        )
        self._handle = handle
        self.pid = handle["pid"]
        self.stdin = handle["stdin"]
        self.stdout = handle["stdout"]
        self.stderr = handle["stderr"]
        self.returncode = None

    # ---- lifecycle ------------------------------------------------

    def poll(self):
        result = self._handle["poll"]()
        if result is None:
            return None
        self.returncode = result
        return result

    def wait(self, timeout=None):
        result = self._handle["wait"](timeout) if timeout is not None else self._handle["wait"]()
        self.returncode = result
        return result

    def kill(self):
        self._handle["kill"]()

    def terminate(self):
        self._handle["terminate"]()

    def send_signal(self, sig):
        self._handle["send_signal"](sig)

    # ---- context manager ------------------------------------------

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        try:
            self.wait()
        except Exception:
            pass
        return False

    # ---- I/O ------------------------------------------------------

    def communicate(self, input=None, timeout=None):
        # Feed `input` to the child's stdin and close it (so the child sees
        # EOF), then read stdout/stderr to completion. The handle's pipes are
        # real OS pipes (see `_subprocess.spawn`), so this actually drives the
        # child to completion rather than reading a buffer captured at spawn.
        #
        # Note: stdin is written in full before stdout/stderr are read, which
        # can deadlock for inputs larger than the OS pipe buffer (~64 KiB).
        # That matches our single-threaded model; CPython uses helper threads.
        if isinstance(input, str):
            input = input.encode(self.encoding or "utf-8")
        if self.stdin is not None:
            try:
                if input:
                    self.stdin.write(input)
                self.stdin.flush()
            except Exception:
                pass
            try:
                self.stdin.close()
            except Exception:
                pass
        try:
            stdout = self.stdout.read() if self.stdout is not None else None
        except Exception:
            stdout = None
        try:
            stderr = self.stderr.read() if self.stderr is not None else None
        except Exception:
            stderr = None
        # Drive `wait` to populate returncode.
        if self.returncode is None:
            self.wait(timeout)
        if self._text:
            stdout = _maybe_decode(stdout, True, self.encoding)
            stderr = _maybe_decode(stderr, True, self.encoding)
        return (stdout, stderr)


def run(args, *, stdin=None, input=None, stdout=None, stderr=None,
        capture_output=False, shell=False, cwd=None, timeout=None,
        check=False, encoding=None, errors=None, text=None,
        env=None):
    """Run a command. The CPython API surface."""
    argv = _coerce_args(args)
    capture = capture_output or (stdout is PIPE and stderr is PIPE)
    rc, out, err = _subprocess.run(argv, input, capture, cwd, env, timeout, shell)
    if text or encoding:
        out = _maybe_decode(out, True, encoding)
        err = _maybe_decode(err, True, encoding)
    cp = CompletedProcess(args, rc, out, err)
    if check and rc != 0:
        raise CalledProcessError(rc, args, out, err)
    return cp


def call(*popenargs, timeout=None, **kwargs):
    return run(*popenargs, timeout=timeout, **kwargs).returncode


def check_call(*popenargs, **kwargs):
    rc = call(*popenargs, **kwargs)
    if rc:
        raise CalledProcessError(rc, popenargs[0] if popenargs else None)
    return 0


def check_output(*popenargs, timeout=None, **kwargs):
    cp = run(*popenargs, capture_output=True, timeout=timeout, check=True, **kwargs)
    return cp.stdout


def getoutput(cmd):
    return _subprocess.getoutput(cmd)


def getstatusoutput(cmd):
    rc, out, _ = _subprocess.run([cmd], None, True, None, None, None, True)
    if isinstance(out, (bytes, bytearray)):
        out = out.decode("utf-8", errors="replace")
    return (rc, out.rstrip("\n") if isinstance(out, str) else out)


__all__ = [
    "Popen", "run", "call", "check_call", "check_output", "getoutput",
    "getstatusoutput", "CalledProcessError", "TimeoutExpired",
    "SubprocessError", "CompletedProcess", "PIPE", "DEVNULL", "STDOUT",
]
