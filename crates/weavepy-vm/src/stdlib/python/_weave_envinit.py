"""Install CPython-style ``os.environ`` / ``os.environb`` (RFC 0040 WS1).

WeavePy's ``os`` module is implemented natively and originally exposed
``environ`` as a plain snapshot ``dict``.  CPython instead exposes
``os.environ`` and ``os.environb`` as ``_Environ`` mappings that

* write straight through to the live process environment via
  ``putenv`` / ``unsetenv`` (so a later ``execv`` inherits the change), and
* share a single *bytes-keyed* backing store, so a write through one view is
  immediately visible through the other.

Importing this module — done exactly once, right after the native ``os``
module is built (see ``Interpreter::load_one``) — upgrades the native module
*in place* to match CPython.  Keeping the mapping in Python (rather than
re-implementing ``MutableMapping`` semantics in Rust) means ``|``, ``|=``,
``copy``, ``setdefault``, ``popitem``, ``repr`` and friends all behave
exactly as the stdlib does.
"""

import os as _os
import sys as _sys
from collections.abc import Mapping, MutableMapping


class _Environ(MutableMapping):
    """Verbatim port of CPython's ``os._Environ``."""

    def __init__(self, data, encodekey, decodekey, encodevalue, decodevalue,
                 putenv, unsetenv):
        self.encodekey = encodekey
        self.decodekey = decodekey
        self.encodevalue = encodevalue
        self.decodevalue = decodevalue
        self.putenv = putenv
        self.unsetenv = unsetenv
        self._data = data

    def __getitem__(self, key):
        try:
            value = self._data[self.encodekey(key)]
        except KeyError:
            # raise KeyError with the original key value
            raise KeyError(key) from None
        return self.decodevalue(value)

    def __setitem__(self, key, value):
        key = self.encodekey(key)
        value = self.encodevalue(value)
        self.putenv(key, value)
        self._data[key] = value

    def __delitem__(self, key):
        encodedkey = self.encodekey(key)
        self.unsetenv(encodedkey)
        try:
            del self._data[encodedkey]
        except KeyError:
            # raise KeyError with the original key value
            raise KeyError(key) from None

    def __iter__(self):
        # list() from dict object is an atomic operation
        keys = list(self._data)
        for key in keys:
            yield self.decodekey(key)

    def __len__(self):
        return len(self._data)

    def __repr__(self):
        formatted_items = ", ".join(
            "{!r}: {!r}".format(self.decodekey(key), self.decodevalue(value))
            for key, value in self._data.items())
        return "environ({{{}}})".format(formatted_items)

    def copy(self):
        return dict(self)

    def setdefault(self, key, value):
        if key not in self:
            self[key] = value
        return self[key]

    def __ior__(self, other):
        self.update(other)
        return self

    def __or__(self, other):
        if not isinstance(other, Mapping):
            return NotImplemented
        new = dict(self)
        new.update(other)
        return new

    def __ror__(self, other):
        if not isinstance(other, Mapping):
            return NotImplemented
        new = dict(other)
        new.update(self)
        return new


def _install():
    try:
        encoding = _sys.getfilesystemencoding() or "utf-8"
    except Exception:
        encoding = "utf-8"

    putenv = _os.putenv
    unsetenv = _os.unsetenv

    def encode(value):
        if not isinstance(value, str):
            raise TypeError("str expected, not %s" % type(value).__name__)
        return value.encode(encoding, "surrogateescape")

    def decode(value):
        return value.decode(encoding, "surrogateescape")

    def encodebytes(value):
        if not isinstance(value, bytes):
            raise TypeError("bytes expected, not %s" % type(value).__name__)
        return value

    # Build the shared, bytes-keyed backing store from the native snapshot.
    raw = getattr(_os, "environ", None)
    data = {}
    items = ()
    if raw is not None:
        try:
            items = list(raw.items())
        except AttributeError:
            items = ()
    for k, v in items:
        bk = k if isinstance(k, bytes) else k.encode(encoding, "surrogateescape")
        bv = v if isinstance(v, bytes) else v.encode(encoding, "surrogateescape")
        data[bk] = bv

    environ = _Environ(data, encode, decode, encode, decode, putenv, unsetenv)
    environb = _Environ(data, encodebytes, bytes, encodebytes, bytes,
                        putenv, unsetenv)

    _os.environ = environ
    _os.environb = environb
    _os.supports_bytes_environ = True

    def getenv(key, default=None):
        """Get an environment variable, return None if it doesn't exist.

        The optional second argument can specify an alternate default.
        key, default and the result are str."""
        return environ.get(key, default)

    def getenvb(key, default=None):
        """Get an environment variable, return None if it doesn't exist.

        The optional second argument can specify an alternate default.
        key, default and the result are bytes."""
        return environb.get(key, default)

    _os.getenv = getenv
    _os.getenvb = getenvb


# ---------------------------------------------------------------------------
# os.py-level helpers that CPython layers on top of the POSIX builtin.
#
# WeavePy implements the low-level syscalls natively (``fork``/``execv`` ...),
# but CPython adds a number of pure-Python conveniences in ``Lib/os.py``:
# ``removedirs``/``renames``, the ``$PATH``-searching ``exec*p[e]`` family,
# the ``spawn*`` family, and ``popen``.  We port them verbatim so argument
# validation, ``$PATH`` search and process spawning behave exactly as the
# stdlib does (and so the ``os`` teardown helpers the test-suite relies on,
# notably ``removedirs``, exist).
# ---------------------------------------------------------------------------


def removedirs(name):
    """removedirs(name) -- super-rmdir, pruning empty parent directories."""
    _os.rmdir(name)
    head, tail = _os.path.split(name)
    if not tail:
        head, tail = _os.path.split(head)
    while head and tail:
        try:
            _os.rmdir(head)
        except OSError:
            break
        head, tail = _os.path.split(head)


def renames(old, new):
    """renames(old, new) -- super-rename, creating/pruning directories."""
    head, tail = _os.path.split(new)
    if head and tail and not _os.path.exists(head):
        _os.makedirs(head)
    _os.rename(old, new)
    head, tail = _os.path.split(old)
    if head and tail:
        try:
            removedirs(head)
        except OSError:
            pass


def get_exec_path(env=None):
    """Directories that will be searched for a named executable (like a shell)."""
    import warnings

    if env is None:
        env = _os.environ

    with warnings.catch_warnings():
        warnings.simplefilter("ignore", BytesWarning)
        try:
            path_list = env.get('PATH')
        except TypeError:
            path_list = None

        if getattr(_os, "supports_bytes_environ", False):
            try:
                path_listb = env[b'PATH']
            except (KeyError, TypeError):
                pass
            else:
                if path_list is not None:
                    raise ValueError(
                        "env cannot contain 'PATH' and b'PATH' keys")
                path_list = path_listb

            if path_list is not None and isinstance(path_list, bytes):
                path_list = _os.fsdecode(path_list)

    if path_list is None:
        path_list = _os.defpath
    return path_list.split(_os.pathsep)


def execl(file, *args):
    """execl(file, *args) -- replace the current process."""
    _os.execv(file, args)


def execle(file, *args):
    """execle(file, *args, env) -- replace the current process."""
    env = args[-1]
    _os.execve(file, args[:-1], env)


def execlp(file, *args):
    """execlp(file, *args) -- $PATH search, replace the current process."""
    execvp(file, args)


def execlpe(file, *args):
    """execlpe(file, *args, env) -- $PATH search with env."""
    env = args[-1]
    execvpe(file, args[:-1], env)


def execvp(file, args):
    """execvp(file, args) -- search $PATH then replace the current process."""
    _execvpe(file, args)


def execvpe(file, args, env):
    """execvpe(file, args, env) -- search $PATH with env then replace."""
    _execvpe(file, args, env)


def _execvpe(file, args, env=None):
    if env is not None:
        exec_func = _os.execve
        argrest = (args, env)
    else:
        exec_func = _os.execv
        argrest = (args,)
        env = _os.environ

    if _os.path.dirname(file):
        exec_func(file, *argrest)
        return
    saved_exc = None
    path_list = get_exec_path(env)
    if _os.name != 'nt':
        file = _os.fsencode(file)
        path_list = map(_os.fsencode, path_list)
    last_exc = None
    for directory in path_list:
        fullname = _os.path.join(directory, file)
        try:
            exec_func(fullname, *argrest)
        except (FileNotFoundError, NotADirectoryError) as e:
            last_exc = e
        except OSError as e:
            last_exc = e
            if saved_exc is None:
                saved_exc = e
    if saved_exc is not None:
        raise saved_exc
    raise last_exc


# spawn*() -- fork()/exec()/waitpid() combos (Unix flavour from os.py).
P_WAIT = 0
P_NOWAIT = P_NOWAITO = 1


def _spawnvef(mode, file, args, env, func):
    if not isinstance(args, (tuple, list)):
        raise TypeError('argv must be a tuple or a list')
    if not args or not args[0]:
        raise ValueError('argv first element cannot be empty')
    pid = _os.fork()
    if not pid:
        # Child
        try:
            if env is None:
                func(file, args)
            else:
                func(file, args, env)
        except:
            _os._exit(127)
    else:
        # Parent
        if mode == P_NOWAIT:
            return pid  # Caller is responsible for waiting!
        while 1:
            wpid, sts = _os.waitpid(pid, 0)
            if _os.WIFSTOPPED(sts):
                continue
            return _os.waitstatus_to_exitcode(sts)


def spawnv(mode, file, args):
    return _spawnvef(mode, file, args, None, _os.execv)


def spawnve(mode, file, args, env):
    return _spawnvef(mode, file, args, env, _os.execve)


def spawnvp(mode, file, args):
    return _spawnvef(mode, file, args, None, execvp)


def spawnvpe(mode, file, args, env):
    return _spawnvef(mode, file, args, env, execvpe)


def spawnl(mode, file, *args):
    return spawnv(mode, file, args)


def spawnle(mode, file, *args):
    env = args[-1]
    return spawnve(mode, file, args[:-1], env)


def spawnlp(mode, file, *args):
    return spawnvp(mode, file, args)


def spawnlpe(mode, file, *args):
    env = args[-1]
    return spawnvpe(mode, file, args[:-1], env)


def popen(cmd, mode="r", buffering=-1):
    """popen(cmd [, mode='r' [, buffering=-1]]) -- open a pipe to/from a command."""
    if not isinstance(cmd, str):
        raise TypeError("invalid cmd type (%s, expected string)" % type(cmd))
    if mode not in ("r", "w"):
        raise ValueError("invalid mode %r" % mode)
    if buffering == 0 or buffering is None:
        raise ValueError("popen() does not support unbuffered streams")
    import subprocess
    if mode == "r":
        proc = subprocess.Popen(cmd,
                                shell=True, text=True,
                                stdout=subprocess.PIPE,
                                bufsize=buffering)
        return _wrap_close(proc.stdout, proc)
    else:
        proc = subprocess.Popen(cmd,
                                shell=True, text=True,
                                stdin=subprocess.PIPE,
                                bufsize=buffering)
        return _wrap_close(proc.stdin, proc)


class _wrap_close:
    """Proxy for a popen() stream whose close() waits for the process."""

    def __init__(self, stream, proc):
        self._stream = stream
        self._proc = proc

    def close(self):
        self._stream.close()
        returncode = self._proc.wait()
        if returncode == 0:
            return None
        if _os.name == 'nt':
            return returncode
        else:
            return returncode << 8  # Shift left to match old behavior

    def __enter__(self):
        return self

    def __exit__(self, *args):
        self.close()

    def __getattr__(self, name):
        return getattr(self._stream, name)

    def __iter__(self):
        return iter(self._stream)


def _install_helpers():
    _os.removedirs = removedirs
    _os.renames = renames
    # Native get_exec_path ignores its ``env`` argument; the os.py port honours
    # it (and the b'PATH' / 'PATH' duplicate-key rule).
    _os.get_exec_path = get_exec_path
    # Native execvp/execvpe reject ``env=None``; the os.py ports search $PATH
    # and fall back to execv when env is None, matching CPython exactly.
    _os.execl = execl
    _os.execle = execle
    _os.execlp = execlp
    _os.execlpe = execlpe
    _os.execvp = execvp
    _os.execvpe = execvpe
    _os._execvpe = _execvpe
    _os.P_WAIT = P_WAIT
    _os.P_NOWAIT = P_NOWAIT
    _os.P_NOWAITO = P_NOWAITO
    _os._spawnvef = _spawnvef
    _os.spawnv = spawnv
    _os.spawnve = spawnve
    _os.spawnvp = spawnvp
    _os.spawnvpe = spawnvpe
    _os.spawnl = spawnl
    _os.spawnle = spawnle
    _os.spawnlp = spawnlp
    _os.spawnlpe = spawnlpe
    _os.popen = popen
    _os._wrap_close = _wrap_close
    # ``os.__all__`` -- CPython exports every public name; the test-suite only
    # requires that staples such as ``open`` and ``walk`` are present.
    try:
        _os.__all__ = sorted(n for n in dir(_os) if not n.startswith("_"))
    except Exception:
        pass


_install()
_install_helpers()
