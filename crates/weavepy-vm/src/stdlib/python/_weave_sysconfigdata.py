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

# CPython's generated `_sysconfigdata` carries *configure-time* paths
# — the base installation, not the running venv (`sysconfig` layers
# venv awareness via its install schemes, not via these vars). Use
# `base_prefix` so `INCLUDEPY`/`LIBDEST`/…, and therefore setuptools'
# `get_python_inc()`, stay truthful inside venvs (RFC 0062 WS2).
_prefix = (
    getattr(sys, "base_prefix", "") or getattr(sys, "prefix", "") or "/usr/local"
)
_exec_prefix = (
    getattr(sys, "base_exec_prefix", "") or getattr(sys, "exec_prefix", _prefix) or _prefix
)
_version_short = "%d.%d" % sys.version_info[:2]
_base_executable = getattr(sys, "_base_executable", "") or getattr(
    sys, "executable", ""
)
_bindir = os.path.dirname(_base_executable) or os.path.join(_prefix, "bin")
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

# RFC 0062 WS2 — compiler variables for building C-extension sdists
# against the installed `{prefix}/include/python3.13/` header tree.
# setuptools' `customize_compiler()` reads CC/CXX/CFLAGS/CCSHARED/
# LDSHARED/LDCXXSHARED/AR/ARFLAGS from here. Extensions never link a
# libpython (same model as a static-libpython CPython): on macOS the
# `-undefined dynamic_lookup` link defers `Py*` resolution to load
# time, on Linux the weavepy binary exports its C-API via
# `--export-dynamic`. Keep in sync with the Rust constants in
# `sysconfig_native.rs` (mirrored into `config-3.13*/Makefile`).
_is_macos = sys.platform == "darwin"
_cflags = "-fno-strict-overflow -Wsign-compare -DNDEBUG -g -O3 -Wall"
if _is_macos:
    _ccshared = ""
    _ldshared = "cc -bundle -undefined dynamic_lookup"
    _ldcxxshared = "c++ -bundle -undefined dynamic_lookup"
else:
    _ccshared = "-fPIC"
    _ldshared = "cc -shared"
    _ldcxxshared = "c++ -shared"

build_time_vars = {
    "ABIFLAGS": "",
    "AR": "ar",
    "ARFLAGS": "rcs",
    "BINDIR": _bindir,
    "BINLIBDEST": os.path.join(_prefix, "lib", "python" + _version_short),
    "CC": "cc",
    "CCSHARED": _ccshared,
    "CFLAGS": _cflags,
    "CONFINCLUDEPY": os.path.join(
        _prefix, "include", "python" + _version_short
    ),
    "CXX": "c++",
    "EXE": "",
    "EXT_SUFFIX": _ext_suffix,
    "HOST_GNU_TYPE": "",
    "INCLUDEPY": os.path.join(_prefix, "include", "python" + _version_short),
    "LDCXXSHARED": _ldcxxshared,
    "LDFLAGS": "",
    "LDLIBRARY": "libpython%s.a" % _version_short,
    # Extension modules must not link against libpython (there is none to
    # link — symbols resolve from the host binary at load time). CPython's
    # static/framework builds publish the empty string here and meson's
    # `links_against_libpython()` keys off it; leaving it unset makes
    # meson add `-lpython3.13` and fail its sysconfig dependency check
    # (numpy test_mem_policy builds a test extension with meson —
    # RFC 0076 WS1).
    "LIBPYTHON": "",
    "LDSHARED": _ldshared,
    "BLDSHARED": _ldshared,
    "LDVERSION": _version_short,
    "OPT": "-DNDEBUG -g -O3 -Wall",
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
