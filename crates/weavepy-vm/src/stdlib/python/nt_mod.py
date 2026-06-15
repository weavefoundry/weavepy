"""Windows ``nt`` OS interface — thin wrapper over WeavePy's ``os`` module.

CPython exposes the low-level Windows syscalls in the ``nt`` builtin and
re-exports them through ``os`` on Windows hosts. WeavePy keeps the bulk of the
implementation inside the Rust-backed ``os`` module and ships this frozen
Python shim so that ``import nt`` works for code (notably ``shutil`` and
``_colorize``) that imports it whenever ``os.name == 'nt'``. It mirrors the
``posix`` shim used on POSIX hosts.
"""

import os as _os
import sys as _sys

# Re-export every public name the underlying ``os`` module advertises so that
# code written against CPython's ``nt`` finds what it expects.
_names = []
for _name in dir(_os):
    if _name.startswith("_"):
        continue
    globals()[_name] = getattr(_os, _name)
    _names.append(_name)


# Access-mode constants (also exposed by ``os``; kept here so ``from nt import
# *`` matches CPython's ``nt``).
F_OK = getattr(_os, "F_OK", 0)
R_OK = getattr(_os, "R_OK", 4)
W_OK = getattr(_os, "W_OK", 2)
X_OK = getattr(_os, "X_OK", 1)

# Spawn-mode constants from CPython's ``nt``.
P_WAIT = 0
P_NOWAIT = 1
P_NOWAITO = 1
P_OVERLAY = 2
P_DETACH = 4

environ = _os.environ
sep = _os.sep
altsep = getattr(_os, "altsep", "/")
linesep = _os.linesep
pathsep = _os.pathsep
defpath = getattr(_os, "defpath", ".;C:\\bin")
devnull = getattr(_os, "devnull", "nul")


# Private helpers a few Windows stdlib modules probe for. Their call sites are
# guarded (``shutil.disk_usage`` / ``_colorize``) and unexercised by this
# build, so provide best-effort stubs that keep ``import nt`` and the common
# attribute lookups working.
def _supports_virtual_terminal():
    return False


def _getdiskusage(path):
    raise OSError("nt._getdiskusage is not implemented on this build of WeavePy")


def getenv(key, default=None):
    if isinstance(key, (bytes, bytearray)):
        key = key.decode()
    return _os.environ.get(key, default)


__all__ = sorted(set(_names) | {
    "F_OK", "R_OK", "W_OK", "X_OK",
    "P_WAIT", "P_NOWAIT", "P_NOWAITO", "P_OVERLAY", "P_DETACH",
    "environ", "sep", "altsep", "linesep", "pathsep", "defpath", "devnull",
})


_sys.modules.setdefault("nt", _sys.modules[__name__])
