"""``os.walk`` — verbatim port of CPython 3.13's ``Lib/os.py`` generator.

WeavePy's ``os`` module is Rust-backed, so its ``walk`` entry point delegates
here. Implementing ``walk`` in Python (exactly as CPython does) gives the
lazy-generator semantics the standard library and ``pathlib``/``shutil``
depend on:

* the caller may mutate ``dirnames`` in place between ``yield``s to prune the
  search (``test_pathlib.test_walk_prune``);
* ``os.scandir`` failures are reported through the ``onerror`` callback
  instead of being silently swallowed (``test_pathlib.test_walk_bad_dir``).

The ``_walk_symlinks_as_files`` sentinel is shared with the Rust ``os`` module
(``os._walk_symlinks_as_files``) so ``pathlib.Path.walk`` — which passes it as
``followlinks`` — compares ``is`` against the same object.
"""

import os as _os
import sys as _sys
from os import scandir, fspath
from os import path

# Share the sentinel object with the native ``os`` module so identity holds.
_walk_symlinks_as_files = _os._walk_symlinks_as_files


def walk(top, topdown=True, onerror=None, followlinks=False):
    """Directory tree generator. See ``help(os.walk)``."""
    _sys.audit("os.walk", top, topdown, onerror, followlinks)

    stack = [fspath(top)]
    islink, join = path.islink, path.join
    while stack:
        top = stack.pop()
        if isinstance(top, tuple):
            yield top
            continue

        dirs = []
        nondirs = []
        walk_dirs = []

        # We may not have read permission for top, in which case we can't
        # get a list of the files the directory contains.  We suppress the
        # exception here, rather than blow up for a minor reason when (say)
        # a thousand readable directories are still left to visit.
        try:
            scandir_it = scandir(top)
        except OSError as error:
            if onerror is not None:
                onerror(error)
            continue

        cont = False
        with scandir_it:
            while True:
                try:
                    try:
                        entry = next(scandir_it)
                    except StopIteration:
                        break
                except OSError as error:
                    if onerror is not None:
                        onerror(error)
                    cont = True
                    break

                try:
                    if followlinks is _walk_symlinks_as_files:
                        is_dir = entry.is_dir(follow_symlinks=False) and not entry.is_junction()
                    else:
                        is_dir = entry.is_dir()
                except OSError:
                    # If is_dir() raises an OSError, consider the entry not to
                    # be a directory, same behaviour as os.path.isdir().
                    is_dir = False

                if is_dir:
                    dirs.append(entry.name)
                else:
                    nondirs.append(entry.name)

                if not topdown and is_dir:
                    # Bottom-up: traverse into sub-directory, but exclude
                    # symlinks to directories if followlinks is False
                    if followlinks:
                        walk_into = True
                    else:
                        try:
                            is_symlink = entry.is_symlink()
                        except OSError:
                            # If is_symlink() raises an OSError, consider the
                            # entry not to be a symbolic link, same behaviour
                            # as os.path.islink().
                            is_symlink = False
                        walk_into = not is_symlink

                    if walk_into:
                        walk_dirs.append(entry.path)
        if cont:
            continue

        if topdown:
            # Yield before sub-directory traversal if going top down
            yield top, dirs, nondirs
            # Traverse into sub-directories
            for dirname in reversed(dirs):
                new_path = join(top, dirname)
                # bpo-23605: os.path.islink() is used instead of caching
                # entry.is_symlink() result during the loop on os.scandir()
                # because the caller can replace the directory entry during
                # the "yield" above.
                if followlinks or not islink(new_path):
                    stack.append(new_path)
        else:
            # Yield after sub-directory traversal if going bottom up
            stack.append((top, dirs, nondirs))
            # Traverse into sub-directories
            for new_path in reversed(walk_dirs):
                stack.append(new_path)
