#!/usr/bin/env python3
"""Regenerate ``crates/weavepy-vm/src/stdlib/cpython314_startup_interned.txt``.

The file lists the identifiers CPython 3.14 has interned before any user
code compiles: the ``_Py_ID`` static-string table, the attribute names of
``builtins``, ``sys``, and the core builtin types, and the identifiers,
qualnames, and name-character string constants of the modules every
process imports at startup (the frozen set plus ``site``'s imports).
``marshal_mod.rs`` consults it to reproduce CPython's ``FLAG_REF`` layout
(see the comments there).

Usage::

    tools/gen_marshal_interned.py [--lib vendor/cpython314/Lib]
                                  [--global-strings pycore_global_strings.h]

Run under CPython 3.14 (it introspects the interpreter). Without
``--global-strings`` the header is fetched from the CPython repository.
"""

from __future__ import annotations

import argparse
import builtins
import os
import re
import sys
import types
import urllib.request

HEADER_URL = (
    "https://raw.githubusercontent.com/python/cpython/3.14/"
    "Include/internal/pycore_global_strings.h"
)

# Python/frozen.c's module list, plus what site imports unconditionally.
STARTUP_MODULES = [
    "importlib/_bootstrap.py",
    "importlib/_bootstrap_external.py",
    "zipimport.py",
    "abc.py",
    "codecs.py",
    "io.py",
    "_collections_abc.py",
    "_sitebuiltins.py",
    "genericpath.py",
    "ntpath.py",
    "posixpath.py",
    "os.py",
    "site.py",
    "stat.py",
    "encodings/__init__.py",
    "encodings/aliases.py",
    "encodings/utf_8.py",
    "importlib/util.py",
    "importlib/machinery.py",
    "runpy.py",
    "__hello__.py",
]

CORE_TYPES = (
    object, type, int, float, complex, str, bytes, bytearray, list, tuple,
    dict, set, frozenset, bool, range, slice, memoryview, property,
    staticmethod, classmethod, super, enumerate, zip, map, filter, reversed,
    BaseException, Exception, OSError, StopIteration, type(None), type(...),
    type(NotImplemented), types.ModuleType, types.FunctionType,
    types.CodeType, types.FrameType, types.MethodType,
    types.BuiltinFunctionType, types.GeneratorType, types.CoroutineType,
    types.CellType, types.MappingProxyType, types.SimpleNamespace,
    types.TracebackType, types.GetSetDescriptorType,
    types.MemberDescriptorType, types.WrapperDescriptorType,
    types.MethodWrapperType, types.MethodDescriptorType,
    types.ClassMethodDescriptorType, type(sys.flags), type(sys.version_info),
    type(sys.implementation), type(iter([])), type(iter(())),
    type({}.keys()), type({}.items()), type({}.values()), type(iter({})),
    type(iter("")), type(iter(b"")), type(iter(range(1))), type(iter(set())),
)

NAME_CHARS = re.compile(r"[A-Za-z0-9_]+")

FILE_HEADER = """\
# Identifiers CPython 3.14 has interned before any user code compiles:
# the `_Py_ID` static-string table (Include/internal/pycore_global_strings.h),
# the attribute names of `builtins`, `sys`, and the core builtin types, and the
# identifiers, qualnames, and name-character string constants of the modules
# every process imports at startup (the frozen set plus site's imports).
# Used by marshal_mod.rs: a string constant equal to one of these has an
# interned twin, which decides whether interning replaces it or interns it in
# place (frozenset constant rebuilds, class-body `__qualname__` constants).
# Regenerate with tools/gen_marshal_interned.py.
"""


def walk(co: types.CodeType, names: set[str]) -> None:
    names.update(co.co_names)
    names.update(co.co_varnames)
    names.update(co.co_cellvars)
    names.update(co.co_freevars)
    names.add(co.co_name)
    names.add(co.co_qualname)
    for c in co.co_consts:
        if isinstance(c, types.CodeType):
            walk(c, names)
        elif isinstance(c, str) and NAME_CHARS.fullmatch(c):
            names.add(c)
        elif isinstance(c, (tuple, frozenset)):
            for e in c:
                if isinstance(e, str) and NAME_CHARS.fullmatch(e):
                    names.add(e)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--lib", default=os.path.join("vendor", "cpython314", "Lib"))
    ap.add_argument("--global-strings", help="path to pycore_global_strings.h")
    ap.add_argument(
        "--out",
        default=os.path.join(
            "crates", "weavepy-vm", "src", "stdlib", "cpython314_startup_interned.txt"
        ),
    )
    args = ap.parse_args()
    if sys.version_info[:2] != (3, 14):
        ap.error("run under CPython 3.14")

    if args.global_strings:
        with open(args.global_strings, encoding="utf-8") as fh:
            header = fh.read()
    else:
        with urllib.request.urlopen(HEADER_URL) as resp:
            header = resp.read().decode("utf-8")
    names: set[str] = set(re.findall(r"STRUCT_FOR_ID\((\w+)\)", header))
    names |= set(dir(builtins)) | set(dir(sys))
    for t in CORE_TYPES:
        names |= set(dir(t))
    for rel in STARTUP_MODULES:
        path = os.path.join(args.lib, rel)
        with open(path, "rb") as fh:
            walk(compile(fh.read(), path, "exec"), names)

    out = sorted(n for n in names if n and "\n" not in n)
    with open(args.out, "w", encoding="utf-8") as fh:
        fh.write(FILE_HEADER + "\n".join(out) + "\n")
    print("%d identifiers -> %s" % (len(out), args.out))
    return 0


if __name__ == "__main__":
    sys.exit(main())
