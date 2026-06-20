"""WeavePy `subprocess` — a CPython-faithful Popen on real fork+exec.

On POSIX this drives `_posixsubprocess.fork_exec` (RFC 0040 WS2/WS3): a
genuine `fork()` + `exec()` in which the child dups the pipe ends onto
stdin/stdout/stderr, applies `close_fds`/`pass_fds`, optional
`start_new_session`/`process_group`, credential switching
(`user`/`group`/`extra_groups`/`umask`), runs an optional `preexec_fn`,
and reports any pre-exec/exec failure to the parent through a
close-on-exec error pipe. The parent reconstructs the original exception
(`FileNotFoundError`, `PermissionError`, …) from that pipe, exactly like
CPython's `subprocess._execute_child`.

`communicate()` uses a `selectors` event loop over non-blocking-bounded
reads/writes (CPython's POSIX `_communicate`), so it never deadlocks on
large output and honours `timeout`.

Where `_posixsubprocess` is unavailable (non-POSIX targets), we fall back
to the legacy `_subprocess.spawn` primitive so the surface still works.

Surface: `Popen`, `run`, `call`, `check_call`, `check_output`,
`getoutput`, `getstatusoutput`, `CompletedProcess`, `CalledProcessError`,
`TimeoutExpired`, `SubprocessError`, and the `PIPE`/`DEVNULL`/`STDOUT`
sentinels.
"""

import sys
import os
import io
import errno
import time
import signal
import builtins
import warnings

_mswindows = (os.name == "nt")

try:
    import _posixsubprocess
    _HAVE_FORK_EXEC = hasattr(_posixsubprocess, "fork_exec") and hasattr(os, "fork")
    # Module-level handle to the native primitive, mirroring CPython's
    # `from _posixsubprocess import fork_exec as _fork_exec`. `_execute_child`
    # calls *this* name so `test_subprocess` can monkeypatch
    # `subprocess._fork_exec` to simulate fork/exec failures.
    _fork_exec = _posixsubprocess.fork_exec if _HAVE_FORK_EXEC else None
except ImportError:
    _posixsubprocess = None
    _HAVE_FORK_EXEC = False
    _fork_exec = None

# CPython exposes these as module-level toggles consulted by `_execute_child`
# and, crucially, by `multiprocessing.util.spawnv_passfds` (which forwards
# `subprocess._USE_VFORK` as the final `allow_vfork` argument to
# `_posixsubprocess.fork_exec`). They must exist as attributes even though our
# native `fork_exec` performs a plain fork/exec regardless of the flag.
_USE_POSIX_SPAWN = False
_USE_VFORK = True

try:
    import _subprocess
except ImportError:
    _subprocess = None

try:
    import selectors as _selectors
except ImportError:
    _selectors = None

if _selectors is not None:
    # The selector `communicate()` uses to multiplex the child pipes. CPython
    # exposes this as `subprocess._PopenSelector` and `test_subprocess`'
    # `ProcessTestCaseNoPoll` swaps it to `SelectSelector` to exercise the
    # non-poll path, so it must be a module-level name that `_communicate`
    # reads dynamically.
    if hasattr(_selectors, "PollSelector"):
        _PopenSelector = _selectors.PollSelector
    else:
        _PopenSelector = _selectors.SelectSelector
else:
    _PopenSelector = None

try:
    import threading as _threading
except ImportError:
    _threading = None

try:
    import select as _select
    _PIPE_BUF = getattr(_select, "PIPE_BUF", 512)
except ImportError:
    _select = None
    _PIPE_BUF = 512


# CPython sentinels.
PIPE = -1
STDOUT = -2
DEVNULL = -3

_PLATFORM_DEFAULT_CLOSE_FDS = object()


# ----------------------------------------------------------------------
# Interpreter-flag reconstruction (used by multiprocessing /
# concurrent.futures to re-launch a faithful child interpreter).
# ----------------------------------------------------------------------

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
    settings in sys.flags, sys.warnoptions and sys._xoptions."""
    flag_opt_map = {
        "debug": "d",
        "dont_write_bytecode": "B",
        "no_site": "S",
        "verbose": "v",
        "bytes_warning": "b",
        "quiet": "q",
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

    if dev_mode:
        args.extend(("-X", "dev"))
    for opt in ("faulthandler", "tracemalloc", "importtime", "showrefcount", "utf8"):
        if opt in xoptions:
            value = xoptions[opt]
            arg = opt if value is True else "%s=%s" % (opt, value)
            args.extend(("-X", arg))

    return args


# ----------------------------------------------------------------------
# Exceptions.
# ----------------------------------------------------------------------

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
        if self.returncode and self.returncode < 0:
            try:
                # Prefer signal.Signals so the message carries the enum repr,
                # e.g. "died with <Signals.SIGABRT: 6>." (CPython's exact text).
                return "Command %r died with %r." % (
                    self.cmd, signal.Signals(-self.returncode))
            except ValueError:
                return "Command %r died with unknown signal %d." % (
                    self.cmd, -self.returncode)
            except AttributeError:
                # signal.Signals enum not available: name the signal directly so
                # the message still says "signal", "SIG<name>" and the number
                # (test_CalledProcessError_str_signal greps for those).
                signum = -self.returncode
                name = _signal_name(signum)
                if name is None:
                    return "Command %r died with unknown signal %d." % (
                        self.cmd, signum)
                return "Command %r died with signal %s (%d)." % (
                    self.cmd, name, signum)
        return "Command %r returned non-zero exit status %d." % (self.cmd, self.returncode)

    @property
    def stdout(self):
        return self.output

    @stdout.setter
    def stdout(self, value):
        self.output = value


class TimeoutExpired(SubprocessError):
    """Raised when a child process times out."""

    def __init__(self, cmd, timeout, output=None, stderr=None):
        self.cmd = cmd
        self.timeout = timeout
        self.output = output
        self.stderr = stderr
        self.args = (cmd, timeout)

    def __str__(self):
        return "Command %r timed out after %s seconds" % (self.cmd, self.timeout)

    @property
    def stdout(self):
        return self.output

    @stdout.setter
    def stdout(self, value):
        self.output = value


class CompletedProcess:
    """The return value of `run(...)`."""

    import types as _types
    __class_getitem__ = classmethod(_types.GenericAlias)
    del _types

    def __init__(self, args, returncode, stdout=None, stderr=None):
        self.args = args
        self.returncode = returncode
        self.stdout = stdout
        self.stderr = stderr

    def __repr__(self):
        parts = ["args={!r}".format(self.args), "returncode={!r}".format(self.returncode)]
        if self.stdout is not None:
            parts.append("stdout={!r}".format(self.stdout))
        if self.stderr is not None:
            parts.append("stderr={!r}".format(self.stderr))
        return "CompletedProcess(%s)" % ", ".join(parts)

    def check_returncode(self):
        if self.returncode:
            raise CalledProcessError(self.returncode, self.args, self.stdout, self.stderr)


# Map a child errno onto the OSError subclass CPython would raise. Our
# Python-level `OSError(errno, ...)` constructor does not auto-select a
# subclass (CPython does this in `OSError.__new__`), so subprocess picks
# the class explicitly when reconstructing a child failure.
_ERRNO_EXCEPTION = {}
for _name, _exc in (
    ("ENOENT", FileNotFoundError),
    ("EACCES", PermissionError),
    ("EPERM", PermissionError),
    ("EEXIST", FileExistsError),
    ("ENOTDIR", NotADirectoryError),
    ("EISDIR", IsADirectoryError),
):
    _num = getattr(errno, _name, None)
    if _num is not None:
        _ERRNO_EXCEPTION[_num] = _exc
del _name, _exc, _num


# ----------------------------------------------------------------------
# Argument coercion.
# ----------------------------------------------------------------------

def _signal_name(signum):
    """Best-effort `SIG*` name for a signal number, used by
    `CalledProcessError.__str__` when `signal.Signals` is unavailable. Prefers
    the canonical name and skips aliases (`SIG_DFL`/`SIG_IGN`, `SIGABRT`==`SIGIOT`)."""
    best = None
    for name in dir(signal):
        if not name.startswith("SIG") or name.startswith("SIG_"):
            continue
        if getattr(signal, name, None) == signum:
            if best is None or len(name) < len(best):
                best = name
    return best


def _fsencode_item(item):
    if isinstance(item, str):
        return item
    if isinstance(item, (bytes, bytearray)):
        return bytes(item).decode(sys.getfilesystemencoding(), "surrogateescape")
    fspath = getattr(item, "__fspath__", None)
    if fspath is not None:
        return _fsencode_item(fspath())
    raise TypeError(
        "argv items must be str, bytes, or os.PathLike, not %s" % type(item).__name__
    )


def _coerce_args(args):
    if isinstance(args, (str, bytes)):
        return [_fsencode_item(args)]
    fspath = getattr(args, "__fspath__", None)
    if fspath is not None:
        return [_fsencode_item(args)]
    return [_fsencode_item(a) for a in args]


def _get_exec_path(env):
    if env is None:
        env = os.environ
    try:
        path_list = env.get("PATH")
    except AttributeError:
        path_list = None
    if path_list is None:
        path_list = getattr(os, "defpath", "/usr/bin:/bin")
    return path_list.split(getattr(os, "pathsep", ":"))


# ----------------------------------------------------------------------
# Reaping bookkeeping (mirrors CPython's `_active` / `_cleanup`).
# ----------------------------------------------------------------------

_active = []


def _cleanup():
    if _active is None:
        return
    for inst in _active[:]:
        res = inst._internal_poll(_deadstate=sys.maxsize)
        if res is not None:
            try:
                _active.remove(inst)
            except ValueError:
                pass


# ----------------------------------------------------------------------
# Popen.
# ----------------------------------------------------------------------

class Popen:
    """A spawned child process, faithful to CPython's `subprocess.Popen`."""

    _child_created = False

    # `Popen[bytes]` / `Popen[str]` produce a `types.GenericAlias` for typing
    # (test_class_getitems), matching CPython's classmethod(GenericAlias).
    import types as _types
    __class_getitem__ = classmethod(_types.GenericAlias)
    del _types

    def __init__(self, args, bufsize=-1, executable=None, stdin=None,
                 stdout=None, stderr=None, preexec_fn=None, close_fds=True,
                 shell=False, cwd=None, env=None, universal_newlines=None,
                 startupinfo=None, creationflags=0, restore_signals=True,
                 start_new_session=False, pass_fds=(), *, user=None,
                 group=None, extra_groups=None, encoding=None, errors=None,
                 text=None, umask=-1, pipesize=-1, process_group=None):
        _cleanup()
        self._waitpid_lock = _threading.Lock() if _threading is not None else _DummyLock()
        self._input = None
        self._input_offset = 0
        self._communication_started = False
        self._fileobj2output = None
        self._devnull = None
        self._closed_child_pipe_fds = False
        # Grace period a ^C-interrupted wait()/communicate()/__exit__ gives the
        # child to exit before resuming the KeyboardInterrupt (bpo-25942). Reset
        # to 0 once spent so subsequent waits in the same teardown don't block.
        self._sigint_wait_secs = 0.25  # 1/xkcd221.getRandomNumber()

        if bufsize is None:
            bufsize = -1
        if not isinstance(bufsize, int):
            raise TypeError("bufsize must be an integer")

        if stdout is STDOUT:
            raise ValueError("STDOUT can only be used for stderr")

        if pipesize is None:
            pipesize = -1
        self.pipesize = pipesize

        self.pid = None
        self.returncode = None
        self.encoding = encoding
        self.errors = errors

        if (text is not None and universal_newlines is not None
                and bool(universal_newlines) != bool(text)):
            raise SubprocessError(
                "Cannot disambiguate when both text and universal_newlines are "
                "supplied but different. Pass one or the other."
            )
        self.text_mode = encoding or errors or text or universal_newlines
        self.args = args

        if not _mswindows:
            # These two are Windows-only knobs; on POSIX CPython rejects them
            # up front rather than silently ignoring (test_invalid_args).
            if startupinfo is not None:
                raise ValueError("startupinfo is only supported on Windows "
                                 "platforms")
            if creationflags != 0:
                raise ValueError("creationflags is only supported on Windows "
                                 "platforms")

        if process_group is None:
            process_group = -1
        if not isinstance(process_group, int):
            raise TypeError("process_group must be an integer or None")

        gid = None
        if group is not None:
            gid = self._resolve_group(group)
        gids = None
        if extra_groups is not None:
            gids = self._resolve_extra_groups(extra_groups)
        uid = None
        if user is not None:
            uid = self._resolve_user(user)
        if umask is None:
            umask = -1

        if pass_fds and not close_fds:
            warnings.warn("pass_fds overriding close_fds.", RuntimeWarning)
            close_fds = True

        (p2cread, p2cwrite,
         c2pread, c2pwrite,
         errread, errwrite,
         to_close) = self._get_handles(stdin, stdout, stderr)

        try:
            if p2cwrite != -1:
                self.stdin = self._wrap_fd(p2cwrite, "w", bufsize)
            else:
                self.stdin = None
            if c2pread != -1:
                self.stdout = self._wrap_fd(c2pread, "r", bufsize)
            else:
                self.stdout = None
            if errread != -1:
                self.stderr = self._wrap_fd(errread, "r", bufsize)
            else:
                self.stderr = None

            self._execute_child(
                args, executable, preexec_fn, close_fds, tuple(pass_fds),
                cwd, env, startupinfo, creationflags, shell,
                p2cread, p2cwrite, c2pread, c2pwrite, errread, errwrite,
                restore_signals, gid, gids, uid, umask,
                start_new_session, process_group, to_close,
            )
        except Exception:
            for f in filter(None, (self.stdin, self.stdout, self.stderr)):
                try:
                    f.close()
                except OSError:
                    pass
            if not self._closed_child_pipe_fds:
                for fd in to_close:
                    try:
                        os.close(fd)
                    except OSError:
                        pass
            raise

    # -- credential resolution (str names go through pwd/grp if present) --

    def _resolve_user(self, user):
        if isinstance(user, int):
            # Mirror CPython's `_Py_Uid_Converter`: uid_t is an unsigned 32-bit
            # integer, validated in the *parent* before the fork. A value past
            # the type's range overflows (`user=2**64` -> OverflowError); an
            # out-of-range value such as -1 raises ValueError, rather than
            # being handed to setuid() in the child and failing with EPERM.
            if user > 2 ** 32 - 1:
                raise OverflowError("user id is greater than maximum")
            if user < 0:
                raise ValueError("user id is not within the allowed range")
            return int(user)
        if isinstance(user, str):
            try:
                import pwd
                return pwd.getpwnam(user).pw_uid
            except (ImportError, KeyError):
                raise ValueError("User name not found: %s" % user)
        raise TypeError("user must be a string, an integer or None")

    def _resolve_group(self, group):
        if isinstance(group, int):
            return group
        if isinstance(group, str):
            try:
                import grp
                return grp.getgrnam(group).gr_gid
            except (ImportError, KeyError):
                raise ValueError("Group name not found: %s" % group)
        raise TypeError("group must be a string, an integer or None")

    def _resolve_extra_groups(self, extra_groups):
        gids = []
        for extra_group in extra_groups:
            if isinstance(extra_group, int):
                # gid_t is an unsigned 32-bit integer on Linux/macOS. Reject
                # values it can't represent (e.g. -1, 2**64) here with
                # ValueError, mirroring CPython's `_Py_Gid_Converter` — the
                # check fires in the parent, before any setgroups() call.
                if not 0 <= extra_group < 2 ** 32:
                    raise ValueError("gid is not within the allowed range")
                gids.append(extra_group)
            elif isinstance(extra_group, str):
                try:
                    import grp
                    gids.append(grp.getgrnam(extra_group).gr_gid)
                except (ImportError, KeyError):
                    raise ValueError("Group name not found: %s" % extra_group)
            else:
                raise TypeError("extra_groups must be integers or strings")
        return gids

    # -- fd wrapping --

    def _wrap_fd(self, fd, kind, bufsize):
        if self.text_mode:
            mode = kind  # 'r' / 'w'
            return io.open(fd, mode, bufsize if bufsize >= 0 else -1,
                           self.encoding, self.errors)
        return io.open(fd, kind + "b", bufsize if bufsize >= 0 else -1)

    def _get_devnull(self):
        if self._devnull is None:
            self._devnull = os.open(getattr(os, "devnull", "/dev/null"),
                                    getattr(os, "O_RDWR", 2))
        return self._devnull

    def _get_handles(self, stdin, stdout, stderr):
        """Construct and return the six descriptors, plus the set of
        child-end fds the parent must close after the fork."""
        p2cread, p2cwrite = -1, -1
        c2pread, c2pwrite = -1, -1
        errread, errwrite = -1, -1
        to_close = set()

        if stdin is None:
            pass
        elif stdin == PIPE:
            p2cread, p2cwrite = os.pipe()
            to_close.add(p2cread)
        elif stdin == DEVNULL:
            p2cread = self._get_devnull()
            to_close.add(p2cread)
        elif isinstance(stdin, int):
            p2cread = stdin
        else:
            p2cread = stdin.fileno()

        if stdout is None:
            pass
        elif stdout == PIPE:
            c2pread, c2pwrite = os.pipe()
            to_close.add(c2pwrite)
        elif stdout == DEVNULL:
            c2pwrite = self._get_devnull()
            to_close.add(c2pwrite)
        elif isinstance(stdout, int):
            c2pwrite = stdout
        else:
            c2pwrite = stdout.fileno()

        if stderr is None:
            pass
        elif stderr == PIPE:
            errread, errwrite = os.pipe()
            to_close.add(errwrite)
        elif stderr == STDOUT:
            if c2pwrite != -1:
                errwrite = c2pwrite
            else:
                errwrite = self._get_stdout_fileno()
        elif stderr == DEVNULL:
            errwrite = self._get_devnull()
            to_close.add(errwrite)
        elif isinstance(stderr, int):
            errwrite = stderr
        else:
            errwrite = stderr.fileno()

        return (p2cread, p2cwrite, c2pread, c2pwrite, errread, errwrite, to_close)

    @staticmethod
    def _get_stdout_fileno():
        try:
            return sys.__stdout__.fileno()
        except (AttributeError, ValueError, io.UnsupportedOperation):
            return 1

    # -- spawn (POSIX fork+exec) --

    def _execute_child(self, args, executable, preexec_fn, close_fds, pass_fds,
                       cwd, env, startupinfo, creationflags, shell,
                       p2cread, p2cwrite, c2pread, c2pwrite, errread, errwrite,
                       restore_signals, gid, gids, uid, umask,
                       start_new_session, process_group, to_close):
        if not _HAVE_FORK_EXEC:
            return self._execute_child_fallback(
                args, shell, cwd, env,
                p2cread, p2cwrite, c2pread, c2pwrite, errread, errwrite, to_close)

        if isinstance(args, (str, bytes)):
            args = [args]
        elif isinstance(args, os.PathLike):
            if shell:
                raise TypeError("path-like args is not allowed when shell is true")
            args = [args]
        else:
            args = list(args)
        args = [_fsencode_item(a) for a in args]

        # Encode `executable` *before* it may replace argv[0] for the shell, so
        # a PathLike (FakePath) executable is decoded to str rather than handed
        # to the native layer as an opaque object (test_pathlike_*).
        if executable is not None:
            executable = _fsencode_item(executable)

        if shell:
            args = ["/bin/sh", "-c"] + args
            if executable:
                args[0] = executable

        if executable is None:
            executable = args[0]
        orig_executable = executable

        # `cwd` may be a PathLike (test_cwd_with_pathlike); fsdecode it so the
        # native chdir target is a plain str/bytes.
        if cwd is not None:
            cwd = _fsencode_item(cwd)

        sys_audit = getattr(sys, "audit", None)
        if sys_audit is not None:
            try:
                sys_audit("subprocess.Popen", executable, args, cwd, env)
            except Exception:
                pass

        if os.path.dirname(executable):
            executable_list = (executable,)
        else:
            executable_list = tuple(
                os.path.join(d, executable) for d in _get_exec_path(env)
            )

        if env is not None:
            env_list = []
            for k, v in env.items():
                k = _fsencode_item(k)
                v = _fsencode_item(v)
                # A literal '=' in the *name* would corrupt the KEY=VALUE
                # encoding the child parses, so reject it (test_invalid_env).
                # Embedded NUL bytes are caught downstream by the native
                # CString conversion, which raises ValueError to match CPython.
                if "=" in k:
                    raise ValueError("illegal environment variable name")
                env_list.append("%s=%s" % (k, v))
        else:
            env_list = None

        # CPython's `_posixsubprocess.fork_exec` refuses to run a `preexec_fn`
        # once the interpreter is tearing down (a `__del__`/`atexit` that spawns
        # a child): the forked child can't safely call back into Python. Mirror
        # that guard here (`test_subprocess.test_preexec_at_exit`).
        if preexec_fn is not None and sys.is_finalizing():
            raise RuntimeError("preexec_fn not supported at interpreter shutdown")

        fds_to_keep = set(int(fd) for fd in pass_fds)
        errpipe_read, errpipe_write = os.pipe()
        fds_to_keep.add(errpipe_write)

        try:
            try:
                self.pid = _fork_exec(
                    args, executable_list,
                    bool(close_fds), tuple(sorted(fds_to_keep)),
                    cwd, env_list,
                    p2cread, p2cwrite, c2pread, c2pwrite,
                    errread, errwrite,
                    errpipe_read, errpipe_write,
                    bool(restore_signals), bool(start_new_session),
                    int(process_group), gid, gids, uid, int(umask),
                    preexec_fn, False,
                )
                # A child now exists (even if it fails to exec, fork_exec
                # returned its pid and we must reap it). Only mark it created
                # *after* a successful return so a pre-fork failure (bad argv,
                # NUL bytes, …) doesn't enrol a pid-less object into `_active`,
                # whose teardown `wait()` would then `os.waitpid(None, …)`.
                self._child_created = True
            finally:
                os.close(errpipe_write)

            self._close_pipe_fds(to_close, p2cread, p2cwrite,
                                 c2pread, c2pwrite, errread, errwrite)

            errpipe_data = bytearray()
            while True:
                part = os.read(errpipe_read, 50000)
                errpipe_data += part
                if not part or len(errpipe_data) > 50000:
                    break
        finally:
            os.close(errpipe_read)

        if errpipe_data:
            # The child failed before/while exec'ing. Reap it so we don't
            # leave a zombie, then reconstruct the original exception.
            try:
                pid, sts = os.waitpid(self.pid, 0)
                if pid == self.pid:
                    self._handle_exitstatus(sts)
                else:
                    self.returncode = sys.maxsize
            except OSError:
                pass

            try:
                exception_name, hex_errno, err_msg = errpipe_data.split(b":", 2)
            except ValueError:
                exception_name = b"SubprocessError"
                hex_errno = b"0"
                err_msg = (b"Bad exception data from child: " + bytes(repr(bytes(errpipe_data)), "ascii"))
            child_exception_type = getattr(
                builtins, exception_name.decode("ascii"), SubprocessError)
            err_msg = err_msg.decode(errors="surrogatepass")
            if issubclass(child_exception_type, OSError) and hex_errno:
                errno_num = int(hex_errno, 16)
                if err_msg == "noexec:chdir":
                    err_msg = ""
                    # The failure was the child's chdir(cwd), so the offending
                    # filename the parent reports is the cwd (test_exception_cwd).
                    err_filename = cwd
                elif err_msg.startswith("noexec"):
                    # A pre-exec setup step failed (setuid/setgid/setpgid): the
                    # child never reached exec, so the error is about the
                    # privilege operation, not the executable file. Report no
                    # filename — `test_user` asserts PermissionError.filename is
                    # None for an EPERM setuid on a non-root host.
                    err_msg = ""
                    err_filename = None
                else:
                    # An exec failure — the filename is the executable we tried
                    # (test_exception_bad_executable / _bad_args_0).
                    err_filename = orig_executable
                if errno_num != 0:
                    err_msg = os.strerror(errno_num)
                    child_exception_type = _ERRNO_EXCEPTION.get(
                        errno_num, child_exception_type)
                if err_filename is not None:
                    raise child_exception_type(errno_num, err_msg, err_filename)
                raise child_exception_type(errno_num, err_msg)
            raise child_exception_type(err_msg)

    def _close_pipe_fds(self, to_close, p2cread, p2cwrite,
                        c2pread, c2pwrite, errread, errwrite):
        # The parent owns the *write* end of stdin and the *read* ends of
        # stdout/stderr; the child ends (collected in `to_close`) are now
        # duplicated in the child and must be closed here.
        devnull_fd = self._devnull
        for fd in to_close:
            if fd == devnull_fd:
                continue
            try:
                os.close(fd)
            except OSError:
                pass
        if devnull_fd is not None:
            try:
                os.close(devnull_fd)
            except OSError:
                pass
        self._closed_child_pipe_fds = True

    def _execute_child_fallback(self, args, shell, cwd, env,
                                p2cread, p2cwrite, c2pread, c2pwrite,
                                errread, errwrite, to_close):
        # Non-POSIX path: drive the legacy `_subprocess.spawn` primitive.
        if _subprocess is None:
            raise SubprocessError("no subprocess backend available")
        argv = _coerce_args(args)
        handle = _subprocess.spawn(
            argv,
            p2cwrite != -1, c2pread != -1, errread != -1,
            errwrite == c2pwrite and c2pwrite != -1,
            cwd, env, bool(shell),
        )
        self._handle = handle
        self.pid = handle["pid"]
        self.stdin = handle["stdin"]
        self.stdout = handle["stdout"]
        self.stderr = handle["stderr"]

    # -- status handling --

    def _handle_exitstatus(self, sts):
        if os.WIFSIGNALED(sts):
            self.returncode = -os.WTERMSIG(sts)
        elif os.WIFEXITED(sts):
            self.returncode = os.WEXITSTATUS(sts)
        elif os.WIFSTOPPED(sts):
            self.returncode = -os.WSTOPSIG(sts)
        else:
            self.returncode = sts

    def poll(self):
        return self._internal_poll()

    def _internal_poll(self, _deadstate=None):
        if self.returncode is not None:
            return self.returncode
        if not _HAVE_FORK_EXEC:
            res = self._handle["poll"]()
            if res is not None:
                self.returncode = res
            return self.returncode
        if self.pid is None:
            return self.returncode
        acquired = self._waitpid_lock.acquire(False)
        if not acquired:
            return self.returncode
        try:
            if self.returncode is not None:
                return self.returncode
            pid, sts = os.waitpid(self.pid, os.WNOHANG)
            if pid == self.pid:
                self._handle_exitstatus(sts)
        except OSError as e:
            if _deadstate is not None:
                self.returncode = _deadstate
            if getattr(e, "errno", None) == getattr(errno, "ECHILD", None):
                self.returncode = 0
        finally:
            self._waitpid_lock.release()
        return self.returncode

    def _try_wait(self, wait_flags):
        try:
            pid, sts = os.waitpid(self.pid, wait_flags)
        except OSError as e:
            if getattr(e, "errno", None) != getattr(errno, "ECHILD", None):
                raise
            pid = self.pid
            sts = 0
        return (pid, sts)

    def wait(self, timeout=None):
        """Wait for child to terminate; set and return :attr:`returncode`."""
        if self.returncode is not None:
            return self.returncode
        if not _HAVE_FORK_EXEC:
            res = self._handle["wait"](timeout) if timeout is not None else self._handle["wait"]()
            self.returncode = res
            return res
        if timeout is not None:
            endtime = time.monotonic() + timeout
        try:
            return self._wait(timeout=timeout)
        except KeyboardInterrupt:
            # bpo-25942: the first ^C waits briefly for the child (assumed to
            # have received the same SIGINT) before re-raising, so a runaway
            # child isn't left behind, yet we never block indefinitely.
            if timeout is not None:
                sigint_timeout = min(self._sigint_wait_secs,
                                     self._remaining_time(endtime))
            else:
                sigint_timeout = self._sigint_wait_secs
            self._sigint_wait_secs = 0  # nothing else should wait.
            try:
                self._wait(timeout=sigint_timeout)
            except TimeoutExpired:
                pass
            raise  # resume the KeyboardInterrupt

    def _wait(self, timeout):
        """Internal implementation of wait() on POSIX.

        Kept as a separate, overridable method because `test_subprocess`
        monkeypatches `Popen._wait` to inject timeouts/`ChildProcessError`.
        """
        if self.returncode is not None:
            return self.returncode

        if timeout is not None:
            endtime = time.monotonic() + timeout
            # Busy-wait with exponential backoff (CPython's strategy): there is
            # no portable "waitpid with timeout", so we poll WNOHANG and sleep.
            delay = 0.0005
            while True:
                if self._waitpid_lock.acquire(False):
                    try:
                        if self.returncode is not None:
                            break
                        pid, sts = self._try_wait(os.WNOHANG)
                        if pid == self.pid:
                            self._handle_exitstatus(sts)
                            break
                    finally:
                        self._waitpid_lock.release()
                remaining = self._remaining_time(endtime)
                if remaining <= 0:
                    raise TimeoutExpired(self.args, timeout)
                delay = min(delay * 2, remaining, 0.05)
                time.sleep(delay)
            return self.returncode

        while self.returncode is None:
            with self._waitpid_lock:
                if self.returncode is not None:
                    break
                pid, sts = self._try_wait(0)
                # waitpid() has been known to return 0 even without WNOHANG
                # in odd situations (bpo-14396); loop until our pid is reaped.
                if pid == self.pid:
                    self._handle_exitstatus(sts)
        return self.returncode

    def _remaining_time(self, endtime):
        if endtime is None:
            return None
        return endtime - time.monotonic()

    # -- I/O --

    def _stdin_write(self, input):
        if input:
            try:
                self.stdin.write(input)
            except BrokenPipeError:
                pass
            except OSError as e:
                if getattr(e, "errno", None) == getattr(errno, "EINVAL", None):
                    pass
                else:
                    raise
        try:
            self.stdin.close()
        except BrokenPipeError:
            pass
        except OSError as e:
            if getattr(e, "errno", None) == getattr(errno, "EINVAL", None):
                pass
            else:
                raise

    def communicate(self, input=None, timeout=None):
        if self._communication_started and input:
            raise ValueError("Cannot send input after starting communication")

        if (timeout is None and not self._communication_started
                and [self.stdin, self.stdout, self.stderr].count(None) >= 2):
            stdout = None
            stderr = None
            if self.stdin:
                self._stdin_write(input)
            elif self.stdout:
                stdout = self.stdout.read()
                self.stdout.close()
            elif self.stderr:
                stderr = self.stderr.read()
                self.stderr.close()
            self.wait()
        else:
            if timeout is not None:
                endtime = time.monotonic() + timeout
            else:
                endtime = None
            try:
                stdout, stderr = self._communicate(input, endtime, timeout)
            except KeyboardInterrupt:
                # bpo-25942: a ^C during communicate() almost certainly also
                # hit the child (shared terminal), so wait a brief, bounded
                # moment for it to exit rather than blocking forever, then let
                # the KeyboardInterrupt resume. `self.wait()` below is *outside*
                # this try, so the propagating ^C skips it (no open-ended wait).
                if timeout is not None:
                    sigint_timeout = min(self._sigint_wait_secs,
                                         self._remaining_time(endtime))
                else:
                    sigint_timeout = self._sigint_wait_secs
                self._sigint_wait_secs = 0  # nothing else should wait.
                try:
                    self._wait(timeout=sigint_timeout)
                except TimeoutExpired:
                    pass
                raise  # resume the KeyboardInterrupt
            finally:
                self._communication_started = True
            self.wait(timeout=self._remaining_time(endtime))

        return (stdout, stderr)

    def _save_input(self, input):
        if self.stdin and self._input is None:
            self._input_offset = 0
            self._input = input
            if input is not None and self.text_mode:
                self._input = self._input.encode(self.encoding or sys.getdefaultencoding(),
                                                 self.errors or "strict")

    def _communicate(self, input, endtime, orig_timeout):
        if self.stdin and not self._communication_started:
            try:
                self.stdin.flush()
            except BrokenPipeError:
                pass  # communicate() must ignore BrokenPipeError.
            except ValueError:
                # stdin may already be closed (test_communicate_stdin_closed_
                # before_call) — tolerate the "closed file" ValueError.
                if not self.stdin.closed:
                    raise
            if not input:
                try:
                    self.stdin.close()
                except BrokenPipeError:
                    pass
                except ValueError:
                    if not self.stdin.closed:
                        raise

        stdout = None
        stderr = None

        if self._fileobj2output is None:
            self._fileobj2output = {}
            if self.stdout:
                self._fileobj2output[self.stdout] = []
            if self.stderr:
                self._fileobj2output[self.stderr] = []

        if self.stdout:
            stdout = self._fileobj2output[self.stdout]
        if self.stderr:
            stderr = self._fileobj2output[self.stderr]

        self._save_input(input)

        if _PopenSelector is None:
            return self._communicate_threaded(stdout, stderr)

        # Drive the pipes non-blocking so a single os.write / os.read can move
        # a large chunk without risking a blocking-write deadlock. CPython
        # instead bounds each write to PIPE_BUF and keeps the fds blocking;
        # that means O(size / PIPE_BUF) selector round-trips, which is far too
        # slow at our (interpreted) selector speed. With O_NONBLOCK the kernel
        # accepts as much as fits and reports the partial count, so a 64 KiB
        # chunk size keeps the round-trip count ~100x lower. Readiness from the
        # selector means EAGAIN should not occur, but we tolerate it anyway.
        _CHUNK = 65536
        for _f in (self.stdin, self.stdout, self.stderr):
            if _f is not None:
                try:
                    os.set_blocking(_f.fileno(), False)
                except (OSError, ValueError, TypeError):
                    # A test may replace a std stream with a mock whose
                    # fileno() is not a real descriptor (test_communicate_
                    # BrokenPipeError_stdin_close_with_timeout); making the
                    # pipe non-blocking is a best-effort fast-path, so a
                    # bogus fileno is simply skipped rather than fatal.
                    pass

        # Track the payload through a byte-oriented view so the offset, the
        # slice and the length check are all in *bytes*. A non-byte memoryview
        # (e.g. array('i', ...)) reports its length in elements, which would
        # desync `_input_offset` (advanced by os.write's byte count) from the
        # completion test — gh-134453. `cast("b")` flattens it to bytes.
        input_view = None
        if self._input:
            if isinstance(self._input, memoryview):
                input_view = self._input.cast("b")
            else:
                input_view = memoryview(self._input)

        # Register stdin whenever buffered input remains — not just when
        # `input` was passed *this* call. After a timeout, `communicate()` may
        # be called again with no new input to flush the unsent tail; keying
        # off `input` there would leave the child blocked forever on
        # `stdin.read()` (it never sees EOF), hanging the continuation. If
        # nothing remains to send, close stdin now so the child sees EOF.
        have_remaining_input = (self.stdin and not self.stdin.closed
                                and input_view is not None
                                and self._input_offset < len(input_view))
        if self.stdin and not self.stdin.closed and not have_remaining_input:
            try:
                self.stdin.close()
            except BrokenPipeError:
                pass

        with _PopenSelector() as selector:
            if have_remaining_input:
                selector.register(self.stdin, _selectors.EVENT_WRITE)
            if self.stdout and not self.stdout.closed:
                selector.register(self.stdout, _selectors.EVENT_READ)
            if self.stderr and not self.stderr.closed:
                selector.register(self.stderr, _selectors.EVENT_READ)

            while selector.get_map():
                timeout = self._remaining_time(endtime)
                if timeout is not None and timeout < 0:
                    self._check_timeout(endtime, orig_timeout,
                                        stdout, stderr, skip_check_and_raise=True)
                ready = selector.select(timeout)
                self._check_timeout(endtime, orig_timeout, stdout, stderr)

                for key, events in ready:
                    if key.fileobj is self.stdin:
                        chunk = input_view[self._input_offset:
                                           self._input_offset + _CHUNK]
                        try:
                            self._input_offset += os.write(key.fd, chunk)
                        except BrokenPipeError:
                            selector.unregister(key.fileobj)
                            key.fileobj.close()
                        except BlockingIOError:
                            pass
                        else:
                            if self._input_offset >= len(input_view):
                                selector.unregister(key.fileobj)
                                key.fileobj.close()
                    elif key.fileobj in (self.stdout, self.stderr):
                        try:
                            data = os.read(key.fd, _CHUNK)
                        except BlockingIOError:
                            continue
                        if not data:
                            selector.unregister(key.fileobj)
                            key.fileobj.close()
                        self._fileobj2output[key.fileobj].append(data)

        # Reap the child within the deadline. `wait` raises with the *remaining*
        # budget, but callers expect the timeout they originally passed (e.g.
        # run(timeout=-1) must read "-1 seconds"), so restore orig_timeout.
        try:
            self.wait(timeout=self._remaining_time(endtime))
        except TimeoutExpired as exc:
            exc.timeout = orig_timeout
            raise

        if stdout is not None:
            stdout = b"".join(stdout)
        if stderr is not None:
            stderr = b"".join(stderr)

        if self.text_mode:
            if stdout is not None:
                stdout = self._translate_newlines(stdout, self.encoding, self.errors)
            if stderr is not None:
                stderr = self._translate_newlines(stderr, self.encoding, self.errors)

        return (stdout, stderr)

    def _communicate_threaded(self, stdout, stderr):
        # Fallback when `selectors` is unavailable: blocking reads. Safe
        # only for the common drain-to-EOF case.
        if self.stdin and self._input:
            try:
                os.write(self.stdin.fileno(), self._input)
            except OSError:
                pass
        if self.stdin:
            try:
                self.stdin.close()
            except OSError:
                pass
        out = b""
        err = b""
        if self.stdout:
            out = self.stdout.read()
            self.stdout.close()
        if self.stderr:
            err = self.stderr.read()
            self.stderr.close()
        self.wait()
        if self.text_mode:
            out = self._translate_newlines(out, self.encoding, self.errors) if self.stdout else None
            err = self._translate_newlines(err, self.encoding, self.errors) if self.stderr else None
        else:
            out = out if self.stdout else None
            err = err if self.stderr else None
        return (out, err)

    def _translate_newlines(self, data, encoding, errors):
        data = data.decode(encoding or sys.getdefaultencoding(), errors or "strict")
        return data.replace("\r\n", "\n").replace("\r", "\n")

    def _check_timeout(self, endtime, orig_timeout, stdout_seq, stderr_seq,
                       skip_check_and_raise=False):
        if endtime is None:
            return
        if skip_check_and_raise or time.monotonic() > endtime:
            raise TimeoutExpired(
                self.args, orig_timeout,
                output=b"".join(stdout_seq) if stdout_seq else None,
                stderr=b"".join(stderr_seq) if stderr_seq else None,
            )

    # -- signals --

    def send_signal(self, sig):
        # bpo-38630: poll first so a process that already exited (but whose
        # status we haven't reaped, leaving returncode None) is detected before
        # we signal — its pid may have been recycled to an unrelated process
        # (test_send_signal_race).
        self.poll()
        if self.returncode is not None:
            return
        if not _HAVE_FORK_EXEC:
            self._handle["send_signal"](int(sig))
            return
        try:
            os.kill(self.pid, sig)
        except ProcessLookupError:
            pass
        except OSError as e:
            if getattr(e, "errno", None) != getattr(errno, "ESRCH", None):
                raise

    def terminate(self):
        self.send_signal(getattr(signal, "SIGTERM", 15))

    def kill(self):
        self.send_signal(getattr(signal, "SIGKILL", 9))

    # -- context manager / finalizer --

    def __enter__(self):
        return self

    def __exit__(self, exc_type, value, traceback):
        if self.stdout:
            try:
                self.stdout.close()
            except OSError:
                pass
        if self.stderr:
            try:
                self.stderr.close()
            except OSError:
                pass
        try:  # Flushing a BufferedWriter may raise an error.
            if self.stdin:
                self.stdin.close()
        except OSError:
            pass
        finally:
            if exc_type == KeyboardInterrupt:
                # bpo-25942: on ^C we assume the child also got the SIGINT and
                # will exit shortly. Wait only a bounded grace period (unless a
                # prior interrupted wait()/communicate() already did) so we
                # neither block forever nor orphan the child, then resume the ^C.
                if self._sigint_wait_secs > 0:
                    try:
                        self._wait(timeout=self._sigint_wait_secs)
                    except TimeoutExpired:
                        pass
                self._sigint_wait_secs = 0  # Note that this has been done.
                return  # resume the KeyboardInterrupt
            # Wait for the process to terminate, to avoid zombies.
            self.wait()

    def __del__(self):
        if not self._child_created:
            return
        if self.returncode is None:
            if _active is not None:
                _active.append(self)

    def __repr__(self):
        obj_repr = "<%s: returncode: %s args: %r>" % (
            self.__class__.__name__, self.returncode, self.args)
        # Keep the repr to a single readable line (test_repr).
        if len(obj_repr) > 80:
            obj_repr = obj_repr[:76] + "...>"
        return obj_repr


class _DummyLock:
    """A no-op lock used when `threading` is unavailable."""

    def acquire(self, blocking=True):
        return True

    def release(self):
        pass

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        return False


# ----------------------------------------------------------------------
# Convenience wrappers.
# ----------------------------------------------------------------------

def run(*popenargs, input=None, capture_output=False, timeout=None,
        check=False, **kwargs):
    if input is not None:
        if kwargs.get("stdin") is not None:
            raise ValueError("stdin and input arguments may not both be used.")
        kwargs["stdin"] = PIPE

    if capture_output:
        if kwargs.get("stdout") is not None or kwargs.get("stderr") is not None:
            raise ValueError("stdout and stderr arguments may not be used with capture_output.")
        kwargs["stdout"] = PIPE
        kwargs["stderr"] = PIPE

    with Popen(*popenargs, **kwargs) as process:
        try:
            stdout, stderr = process.communicate(input, timeout=timeout)
        except TimeoutExpired:
            process.kill()
            stdout, stderr = process.communicate()
            raise
        except:  # noqa: E722 - re-raise after cleanup, like CPython.
            process.kill()
            raise
        retcode = process.poll()
    if check and retcode:
        raise CalledProcessError(retcode, process.args, output=stdout, stderr=stderr)
    return CompletedProcess(process.args, retcode, stdout, stderr)


def call(*popenargs, timeout=None, **kwargs):
    with Popen(*popenargs, **kwargs) as p:
        try:
            return p.wait(timeout=timeout)
        except:  # noqa: E722 - including KeyboardInterrupt, wait handled that.
            p.kill()
            # We don't call p.wait() again as p.__exit__ does that for us.
            raise


def check_call(*popenargs, **kwargs):
    retcode = call(*popenargs, **kwargs)
    if retcode:
        cmd = kwargs.get("args")
        if cmd is None:
            cmd = popenargs[0]
        raise CalledProcessError(retcode, cmd)
    return 0


def check_output(*popenargs, timeout=None, **kwargs):
    if "stdout" in kwargs:
        raise ValueError("stdout argument not allowed, it will be overridden.")
    if "check" in kwargs:
        raise ValueError("check argument not allowed, it will be overridden.")
    if "input" in kwargs and kwargs["input"] is None:
        # Legacy: check_output(input=None) means "empty input". The empty
        # value must match the stream type, so honour *every* text-mode
        # switch — including `universal_newlines` — or `_save_input` will try
        # to `.encode()` a bytes object in text mode.
        if (kwargs.get("encoding") or kwargs.get("errors")
                or kwargs.get("text") or kwargs.get("universal_newlines")):
            kwargs["input"] = ""
        else:
            kwargs["input"] = b""
    return run(*popenargs, stdout=PIPE, timeout=timeout, check=True, **kwargs).stdout


def list2cmdline(seq):
    """Translate a sequence of arguments into a command line string using the
    MS C runtime quoting rules (kept cross-platform for parity with CPython;
    intentionally excluded from ``__all__``)."""
    result = []
    needquote = False
    for arg in map(os.fsdecode, seq):
        bs_buf = []
        if result:
            result.append(' ')
        needquote = (" " in arg) or ("\t" in arg) or not arg
        if needquote:
            result.append('"')
        for c in arg:
            if c == '\\':
                bs_buf.append(c)
            elif c == '"':
                result.append('\\' * len(bs_buf) * 2)
                bs_buf = []
                result.append('\\"')
            else:
                if bs_buf:
                    result.extend(bs_buf)
                    bs_buf = []
                result.append(c)
        if bs_buf:
            result.extend(bs_buf)
        if needquote:
            result.extend(bs_buf)
            result.append('"')
    return ''.join(result)


def getstatusoutput(cmd, *, encoding=None, errors=None):
    try:
        data = check_output(cmd, shell=True, text=True, stderr=STDOUT,
                            encoding=encoding, errors=errors)
        exitcode = 0
    except CalledProcessError as ex:
        data = ex.output
        exitcode = ex.returncode
    if data and data[-1:] == "\n":
        data = data[:-1]
    return exitcode, data


def getoutput(cmd, *, encoding=None, errors=None):
    return getstatusoutput(cmd, encoding=encoding, errors=errors)[1]


__all__ = [
    "Popen", "run", "call", "check_call", "check_output", "getoutput",
    "getstatusoutput", "CalledProcessError", "TimeoutExpired",
    "SubprocessError", "CompletedProcess", "PIPE", "DEVNULL", "STDOUT",
]
