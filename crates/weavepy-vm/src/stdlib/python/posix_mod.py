"""POSIX-style OS interface — thin wrapper over WeavePy's ``os`` module.

CPython exposes the low-level POSIX syscalls in the ``posix`` builtin and
re-exports them through ``os`` on POSIX hosts. WeavePy keeps the bulk of
the implementation inside the Rust-backed ``os`` module and ships this
frozen Python shim so that ``import posix`` works for code (notably
``multiprocessing``, ``runpy``, the ``test`` package) that relies on the
classical layout.
"""

import os as _os
import sys as _sys

# Re-export every name the underlying ``os`` module advertises so that
# code written against CPython's ``posix`` finds what it expects.
_names = []
for _name in dir(_os):
    if _name.startswith("_"):
        continue
    globals()[_name] = getattr(_os, _name)
    _names.append(_name)


# macOS `fcopyfile(3)` fast-copy hooks live behind a leading underscore, so
# the ``dir(_os)`` loop above skips them — re-export explicitly so the bundled
# ``shutil`` (and ``test_shutil.TestZeroCopyMACOS``) find ``posix._fcopyfile``
# and the ``_COPYFILE_*`` flag bits exactly where CPython's ``posix`` puts them.
if hasattr(_os, "_fcopyfile"):
    _fcopyfile = _os._fcopyfile
    _COPYFILE_ACL = _os._COPYFILE_ACL
    _COPYFILE_STAT = _os._COPYFILE_STAT
    _COPYFILE_XATTR = _os._COPYFILE_XATTR
    _COPYFILE_DATA = _os._COPYFILE_DATA

# A couple of POSIX-only constants that ``os`` may not expose
# explicitly (kept here so ``from posix import *`` matches CPython).
F_OK = getattr(_os, "F_OK", 0)
R_OK = getattr(_os, "R_OK", 4)
W_OK = getattr(_os, "W_OK", 2)
X_OK = getattr(_os, "X_OK", 1)

# Spawn-related constants live on ``os`` in CPython too, but they sit in
# the ``posix`` namespace for legacy callers.
P_WAIT = 0
P_NOWAIT = 1
P_NOWAITO = 1

# Process exit constants used by ``waitpid``/``wait`` consumers.
WNOHANG = getattr(_os, "WNOHANG", 1)
WUNTRACED = getattr(_os, "WUNTRACED", 2)
WCONTINUED = getattr(_os, "WCONTINUED", 8)
WIFEXITED = getattr(_os, "WIFEXITED", lambda status: True)
WEXITSTATUS = getattr(_os, "WEXITSTATUS", lambda status: status & 0xFF)
WIFSIGNALED = getattr(_os, "WIFSIGNALED", lambda status: False)
WTERMSIG = getattr(_os, "WTERMSIG", lambda status: status & 0x7F)
WIFSTOPPED = getattr(_os, "WIFSTOPPED", lambda status: False)
WSTOPSIG = getattr(_os, "WSTOPSIG", lambda status: 0)
WCOREDUMP = getattr(_os, "WCOREDUMP", lambda status: False)


def _missing(name):
    def _fail(*_a, **_kw):
        raise OSError(f"posix.{name} is not implemented on this build of WeavePy")

    _fail.__name__ = name
    return _fail


for _candidate in ("fork", "forkpty", "wait", "waitpid", "wait3", "wait4",
                   "execv", "execve", "execvp", "execvpe", "spawnv", "spawnvp",
                   "spawnvpe", "spawnve", "kill", "killpg", "setsid", "setpgid",
                   "tcgetpgrp", "tcsetpgrp"):
    if _candidate not in globals():
        globals()[_candidate] = _missing(_candidate)
        _names.append(_candidate)


environ = _os.environ
sep = _os.sep
linesep = _os.linesep
defpath = getattr(_os, "defpath", "/bin:/usr/bin")
altsep = getattr(_os, "altsep", None)
pathsep = _os.pathsep
devnull = getattr(_os, "devnull", "/dev/null")


# Names matching CPython's ``posix.__all__``.
__all__ = sorted(set(_names) | {
    "F_OK", "R_OK", "W_OK", "X_OK",
    "P_WAIT", "P_NOWAIT", "P_NOWAITO",
    "WNOHANG", "WUNTRACED", "WCONTINUED",
    "WIFEXITED", "WEXITSTATUS", "WIFSIGNALED", "WTERMSIG",
    "WIFSTOPPED", "WSTOPSIG", "WCOREDUMP",
    "environ", "sep", "linesep", "defpath", "altsep", "pathsep", "devnull",
})


# CPython's ``posix.environ`` is a plain ``dict``; ours is whatever
# ``os.environ`` is. For compatibility code that does
# ``posix.environ[b'PATH']`` (bytes keys), provide a tolerant accessor.
def getenv(key, default=None):
    if isinstance(key, (bytes, bytearray)):
        key = key.decode()
    return _os.environ.get(key, default)


_sys.modules.setdefault("posix", _sys.modules[__name__])
