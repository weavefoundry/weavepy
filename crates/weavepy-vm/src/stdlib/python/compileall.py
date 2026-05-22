"""Public ``compileall`` module (RFC 0019).

Walks a directory and compiles every reachable ``.py`` file into a
matching ``.pyc`` via ``py_compile``. Exposes the CPython-compatible
``compile_dir`` / ``compile_file`` / ``compile_path`` entry points.
"""

import os
import sys

import py_compile


def compile_file(fullname, ddir=None, force=False, rx=None, quiet=0,
                 legacy=False, optimize=-1, invalidation_mode=None,
                 *, stripdir=None, prependdir=None, limit_sl_dest=None,
                 hardlink_dupes=False):
    if rx is not None and rx.search(fullname):
        return True
    cfile = py_compile.cache_from_source(fullname)
    if not force and os.path.exists(cfile):
        try:
            src_mtime = os.path.getmtime(fullname)
            cache_mtime = os.path.getmtime(cfile)
            if cache_mtime >= src_mtime:
                return True
        except OSError:
            pass
    if quiet < 2:
        print("Compiling %r..." % fullname)
    try:
        py_compile.compile(fullname, cfile, doraise=True)
        return True
    except py_compile.PyCompileError as e:
        if quiet < 2:
            print("    failed: %s" % e)
        return False


def compile_dir(dir, maxlevels=None, ddir=None, force=False, rx=None,
                quiet=0, legacy=False, optimize=-1, workers=1,
                invalidation_mode=None, *, stripdir=None,
                prependdir=None, limit_sl_dest=None, hardlink_dupes=False):
    if maxlevels is None:
        maxlevels = 10
    success = True
    if maxlevels < 0:
        return success
    if quiet < 2:
        print("Listing %r..." % dir)
    try:
        names = sorted(os.listdir(dir))
    except OSError:
        if quiet < 2:
            print("Can't list %r" % dir)
        return False
    for name in names:
        if name == "__pycache__":
            continue
        fullname = os.path.join(dir, name)
        if os.path.isdir(fullname):
            if not compile_dir(fullname, maxlevels - 1, ddir, force, rx,
                                quiet, legacy, optimize, workers,
                                invalidation_mode):
                success = False
        elif fullname.endswith(".py"):
            if not compile_file(fullname, ddir, force, rx, quiet, legacy,
                                optimize, invalidation_mode):
                success = False
    return success


def compile_path(skip_curdir=1, maxlevels=0, force=False, quiet=0,
                 legacy=False, optimize=-1, invalidation_mode=None):
    success = True
    for dir in sys.path:
        if (not dir or dir == os.curdir) and skip_curdir:
            if quiet < 2:
                print("Skipping current directory")
        else:
            if not compile_dir(dir, maxlevels, None, force, None, quiet,
                                legacy, optimize, invalidation_mode):
                success = False
    return success


def main(args=None):
    if args is None:
        args = sys.argv[1:]
    if not args:
        compile_path()
        return
    for arg in args:
        if os.path.isdir(arg):
            compile_dir(arg)
        else:
            compile_file(arg)


if __name__ == "__main__":
    main()


__all__ = ["compile_dir", "compile_file", "compile_path", "main"]
