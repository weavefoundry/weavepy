"""WeavePy's `_sysconfigdata` — the build-time variables behind the
verbatim CPython `sysconfig` package (RFC 0053 WS4).

CPython generates this module during its build (`_generate_posix_vars`
writes ~700 Makefile variables). WeavePy has no autoconf build, so the
honest equivalent is computed here from the running interpreter: real
prefixes, the WeavePy SOABI/cache tag, and the capability flags the
conformance surface reads (`HAVE_GETENTROPY`, `Py_GIL_DISABLED`, …).

Registered under the platform-derived names `sysconfig` looks for
(`_sysconfigdata_{abiflags}_{platform}_{multiarch}`).
"""

import os
import sys

_prefix = getattr(sys, "prefix", "") or "/usr/local"
_exec_prefix = getattr(sys, "exec_prefix", _prefix) or _prefix
_version_short = "%d.%d" % sys.version_info[:2]
_bindir = os.path.dirname(getattr(sys, "executable", "") or "") or os.path.join(
    _prefix, "bin"
)

build_time_vars = {
    "ABIFLAGS": "",
    "AR": "ar",
    "ARFLAGS": "rcs",
    "BINDIR": _bindir,
    "BINLIBDEST": os.path.join(_prefix, "lib", "python" + _version_short),
    "CC": "cc",
    "CFLAGS": "",
    "CONFINCLUDEPY": os.path.join(
        _prefix, "include", "python" + _version_short
    ),
    "CXX": "c++",
    "EXE": "",
    "EXT_SUFFIX": ".so",
    "HOST_GNU_TYPE": "",
    "INCLUDEPY": os.path.join(_prefix, "include", "python" + _version_short),
    "LDFLAGS": "",
    "LDLIBRARY": "",
    "LDSHARED": "cc -shared",
    "LDVERSION": _version_short,
    "LIBDEST": os.path.join(_prefix, "lib", "python" + _version_short),
    "LIBDIR": os.path.join(_prefix, "lib"),
    "LIBRARY": "",
    "MULTIARCH": "",
    "Py_DEBUG": 0,
    "Py_ENABLE_SHARED": 0,
    "Py_GIL_DISABLED": 0,
    "SHLIB_SUFFIX": ".so",
    "SIZEOF_VOID_P": 8,
    "SOABI": "weavepy-313",
    "TZPATH": "/usr/share/zoneinfo:/usr/lib/zoneinfo:/usr/share/lib/zoneinfo:/etc/zoneinfo",
    "VERSION": _version_short,
    "WITH_DOC_STRINGS": 1,
    "WITH_PYMALLOC": 0,
    "exec_prefix": _exec_prefix,
    "platlibdir": "lib",
    "prefix": _prefix,
}

# Randomness-source capability flags, matching the host platform.
# `os.urandom` is implemented fd-free over `getentropy`/`getrandom`
# (see `os.rs`); `test_os.URandomFDTests` skips when either is set.
_plat = getattr(sys, "platform", "")
if _plat == "darwin" or _plat.startswith(("freebsd", "openbsd", "netbsd")):
    build_time_vars["HAVE_GETENTROPY"] = 1
elif _plat.startswith("linux"):
    build_time_vars["HAVE_GETRANDOM_SYSCALL"] = 1
    build_time_vars["HAVE_GETRANDOM"] = 1
    build_time_vars["HAVE_GETENTROPY"] = 1
