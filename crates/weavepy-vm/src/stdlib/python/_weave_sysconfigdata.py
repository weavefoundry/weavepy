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
# RFC 0055 WS1 — ABI identity. WeavePy's binary ABI loads stock
# CPython 3.13 extensions, so EXT_SUFFIX/SOABI carry CPython's tags
# (they must equal `_imp.extension_suffixes()[0]` — asserted by
# test_sysconfig — and they are what setuptools/packaging use to name
# and match built extensions).
_multiarch = getattr(sys.implementation, "_multiarch", "")
if _multiarch:
    _soabi = "cpython-%d%d-%s" % (*sys.version_info[:2], _multiarch)
else:
    _soabi = "cpython-%d%d" % sys.version_info[:2]
_ext_suffix = "." + _soabi + ".so"
# `{stdlib}/config-3.13-{multiarch}` — materialized by the stdlib
# tree (RFC 0055) so `get_makefile_filename()`/`srcdir` point at real
# files.
_config_dir = os.path.join(
    _prefix,
    "lib",
    "python" + _version_short,
    "config-%s-%s" % (_version_short, _multiarch)
    if _multiarch
    else "config-" + _version_short,
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
    "EXT_SUFFIX": _ext_suffix,
    "HOST_GNU_TYPE": "",
    "INCLUDEPY": os.path.join(_prefix, "include", "python" + _version_short),
    "LDFLAGS": "",
    "LDLIBRARY": "libpython%s.a" % _version_short,
    "LDSHARED": "cc -shared",
    "LDVERSION": _version_short,
    "LIBDEST": os.path.join(_prefix, "lib", "python" + _version_short),
    "LIBDIR": os.path.join(_prefix, "lib"),
    "LIBRARY": "libpython%s.a" % _version_short,
    "MULTIARCH": _multiarch,
    "Py_DEBUG": 0,
    "Py_ENABLE_SHARED": 0,
    "Py_GIL_DISABLED": 0,
    "SHLIB_SUFFIX": ".so",
    "SIZEOF_VOID_P": 8,
    "SOABI": _soabi,
    "srcdir": _config_dir,
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
