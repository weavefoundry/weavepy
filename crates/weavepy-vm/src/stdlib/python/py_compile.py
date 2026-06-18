"""Public ``py_compile`` module (RFC 0019).

Compiles a single ``.py`` file to a ``.pyc`` bytecode archive that
``compileall`` and the WeavePy import machinery understand.

The framing matches CPython's PEP-552 magic-tag-based layout: a
16-byte header followed by a ``marshal.dumps`` of the code object.
RFC 0033 adopts CPython 3.13's magic number; WeavePy's distinct
cache tag (``weavepy-3.13``) keeps its ``.pyc`` files from colliding
with CPython's ``cpython-313`` artifacts.

Layout (little-endian):

* 4 bytes — magic number (CPython 3.13's ``b"\\xf3\\r\\r\\n"``).
* 4 bytes — flags (currently always 0).
* 4 bytes — source mtime (truncated to 32 bits).
* 4 bytes — source size (truncated to 32 bits).
"""

import marshal
import os
import struct

MAGIC_NUMBER = b"\xf3\x0d\x0d\x0a"  # CPython 3.13 bytecode magic (RFC 0033)


class PyCompileError(Exception):
    def __init__(self, exc_type, exc_value, file, msg=""):
        super().__init__(msg or "%s: %s in %r" % (exc_type, exc_value, file))
        self.exc_type_name = exc_type
        self.exc_value = exc_value
        self.file = file
        self.msg = msg


def _cache_from_source(path, optimization=""):
    head, tail = os.path.split(path)
    if tail.endswith(".py"):
        tail = tail[:-3]
    suffix = "" if not optimization else "." + str(optimization)
    cache_dir = os.path.join(head, "__pycache__")
    return os.path.join(cache_dir, "%s.weavepy-3.13%s.pyc" % (tail, suffix))


def cache_from_source(path, optimization=""):
    return _cache_from_source(path, optimization)


def compile(file, cfile=None, dfile=None, doraise=False, optimize=-1,
            invalidation_mode=None, quiet=0):
    """Byte-compile *file* into a ``.pyc`` next to it (or in cfile).

    Mirrors CPython's ``py_compile.compile``: read the source, compile it
    with the built-in compiler at the requested optimization level (wrapping
    any compile error in :class:`PyCompileError`), then write the PEP-552
    timestamp ``.pyc`` framing the WeavePy loader understands.
    """
    import builtins
    if cfile is None:
        if optimize >= 0:
            opt = "" if optimize == 0 else optimize
            cfile = _cache_from_source(file, optimization=opt)
        else:
            cfile = _cache_from_source(file)
    try:
        with open(file, "r", encoding="utf-8") as f:
            source = f.read()
        st = os.stat(file)
        mtime = int(st.st_mtime)
        size = int(st.st_size)
    except OSError as e:
        if doraise:
            raise PyCompileError(type(e).__name__, e, file)
        if quiet < 2:
            print("py_compile: skipping %r: %s" % (file, e))
        return None
    # The real compile step is the interpreter's built-in `compile`, exactly
    # as CPython's `SourceFileLoader.source_to_code` ultimately calls. A
    # SyntaxError (etc.) is reported as a PyCompileError so callers like
    # `zipfile.PyZipFile` can fall back to shipping the raw `.py`.
    try:
        code = builtins.compile(source, dfile or file, "exec", optimize=optimize)
    except Exception as err:
        py_exc = PyCompileError(
            type(err).__name__,
            err,
            dfile or file,
            "%s: %s" % (type(err).__name__, err),
        )
        if doraise:
            raise py_exc
        if quiet < 2:
            print(py_exc.msg)
        return None
    try:
        os.makedirs(os.path.dirname(cfile), exist_ok=True)
        with open(cfile, "wb") as f:
            f.write(MAGIC_NUMBER)
            f.write(struct.pack("<I", 0))
            f.write(struct.pack("<I", mtime & 0xFFFFFFFF))
            f.write(struct.pack("<I", size & 0xFFFFFFFF))
            f.write(marshal.dumps(code))
    except OSError as e:
        if doraise:
            raise PyCompileError(type(e).__name__, e, file)
        if quiet < 2:
            print("py_compile: skipping %r: %s" % (file, e))
        return None
    return cfile


def main(args=None):
    import sys
    if args is None:
        args = sys.argv[1:]
    for arg in args:
        try:
            compile(arg, doraise=True)
        except PyCompileError as e:
            print(e)


if __name__ == "__main__":
    main()


__all__ = ["compile", "PyCompileError", "MAGIC_NUMBER",
           "cache_from_source"]
