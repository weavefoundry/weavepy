"""WeavePy ``zipimport`` — import Python modules from ZIP archives (PEP 273).

CPython's ``zipimport`` is implemented on top of the frozen
``_frozen_importlib_external`` private API, which WeavePy does not ship. This
is a self-contained reimplementation of the same *public* surface, built on
WeavePy's pure-Python ``zipfile`` reader (which itself sits on the native
``zlib``). A :class:`zipimporter` plugs into ``sys.path_hooks`` and implements
the PEP 451 finder/loader protocol used by both the Python ``find_spec`` path
(``runpy``/``pkgutil``/``importlib.util``) and WeavePy's Rust import loader
(which falls back to ``sys.meta_path`` for entries it can't resolve natively).

Supported entries inside the archive: ``<mod>.py`` / ``<mod>.pyc`` and the
package forms ``<pkg>/__init__.py`` / ``<pkg>/__init__.pyc``. ``.pyc`` entries
are unmarshalled directly (WeavePy bytecode, 16-byte header).
"""

import sys
import os
import marshal


__all__ = ["ZipImportError", "zipimporter"]


class ZipImportError(ImportError):
    """Raised by :class:`zipimporter` when a path is not a usable archive.

    A subclass of :class:`ImportError` so ``PathFinder`` treats a failed
    ``sys.path_hooks`` probe as "not my path, try the next hook".
    """


# archive path -> (zipfile.ZipFile, frozenset(names)). Mirrors CPython's
# module-level ``_zip_directory_cache`` so repeated lookups against the same
# archive don't re-parse its central directory.
_zip_directory_cache = {}


def _read_directory(archive):
    """Open *archive* (a real file) as a zip and cache its name table."""
    entry = _zip_directory_cache.get(archive)
    if entry is not None:
        return entry
    import zipfile
    try:
        zf = zipfile.ZipFile(archive)
    except OSError as exc:
        raise ZipImportError("can't open Zip file: %r" % (archive,),
                             path=archive) from exc
    except Exception as exc:
        # zipfile raises ``BadZipFile`` (a ``ValueError`` subclass) for a
        # file that exists but isn't a valid archive.
        raise ZipImportError("not a Zip file: %r" % (archive,),
                             path=archive) from exc
    entry = (zf, frozenset(zf.namelist()))
    _zip_directory_cache[archive] = entry
    return entry


class zipimporter:
    """A :pep:`451` finder/loader for one location inside a zip archive.

    *path* is either the archive itself (``app.zip``) or a sub-path within it
    (``app.zip/pkg``); the constructor splits it into the on-disk ``archive``
    and an internal ``prefix`` (``''`` or ``'pkg/'``).
    """

    def __init__(self, path):
        if not isinstance(path, str):
            path = os.fsdecode(path)
        if not path:
            raise ZipImportError("archive path is empty", path=path)
        # Find the real archive file by walking up the path; everything below
        # it becomes the in-archive prefix.
        archive = path
        prefix_parts = []
        while True:
            if os.path.isfile(archive):
                break
            head, tail = os.path.split(archive)
            if not tail or head == archive:
                raise ZipImportError("not a Zip file: %r" % (path,), path=path)
            prefix_parts.append(tail)
            archive = head
        _read_directory(archive)  # validates it's a real zip (caches it)
        self.archive = archive
        prefix_parts.reverse()
        self.prefix = ("/".join(prefix_parts) + "/") if prefix_parts else ""

    def __repr__(self):
        loc = self.archive
        if self.prefix:
            loc = os.path.join(self.archive, self.prefix)
        return "<zipimporter object %r>" % (loc,)

    # -- archive helpers ------------------------------------------------

    def _names(self):
        return _read_directory(self.archive)[1]

    def _zipfile(self):
        return _read_directory(self.archive)[0]

    def _find(self, fullname):
        """Locate *fullname* inside the archive.

        Returns ``(relpath, is_package, is_bytecode)`` or ``None``. Package
        forms (``<base>/__init__``) win over plain modules, and source
        (``.py``) wins over bytecode (``.pyc``), matching CPython's order.
        """
        tail = fullname.rpartition(".")[2]
        base = self.prefix + tail
        names = self._names()
        for suffix, is_bytecode in ((".py", False), (".pyc", True)):
            cand = base + "/__init__" + suffix
            if cand in names:
                return (cand, True, is_bytecode)
        for suffix, is_bytecode in ((".py", False), (".pyc", True)):
            cand = base + suffix
            if cand in names:
                return (cand, False, is_bytecode)
        return None

    def _read(self, relpath):
        return self._zipfile().read(relpath)

    # -- PEP 451 finder -------------------------------------------------

    def find_spec(self, fullname, target=None):
        found = self._find(fullname)
        if found is None:
            return None
        relpath, is_package, _is_bytecode = found
        from importlib.machinery import ModuleSpec
        origin = os.path.join(self.archive, relpath)
        spec = ModuleSpec(fullname, self, origin=origin, is_package=is_package)
        spec.has_location = True
        if is_package:
            tail = fullname.rpartition(".")[2]
            pkg_path = os.path.join(self.archive, self.prefix + tail)
            spec.submodule_search_locations = [pkg_path]
        return spec

    def find_module(self, fullname, path=None):
        # Legacy (pre-PEP 451) finder API, still consulted by a few tools.
        return self if self._find(fullname) is not None else None

    def find_loader(self, fullname, path=None):
        if self._find(fullname) is not None:
            return self, []
        return None, []

    # -- PEP 302 / 451 loader ------------------------------------------

    def get_code(self, fullname):
        found = self._find(fullname)
        if found is None:
            raise ZipImportError("can't find module %r" % (fullname,),
                                 name=fullname)
        relpath, _is_package, is_bytecode = found
        data = self._read(relpath)
        if is_bytecode:
            if len(data) < 16:
                raise ZipImportError("bad pyc data for %r" % (fullname,),
                                     name=fullname)
            return marshal.loads(data[16:])
        origin = os.path.join(self.archive, relpath)
        return compile(data.decode("utf-8"), origin, "exec")

    def get_source(self, fullname):
        found = self._find(fullname)
        if found is None:
            raise ZipImportError("can't find module %r" % (fullname,),
                                 name=fullname)
        relpath, _is_package, is_bytecode = found
        if is_bytecode:
            return None
        return self._read(relpath).decode("utf-8")

    def get_filename(self, fullname):
        found = self._find(fullname)
        if found is None:
            raise ZipImportError("can't find module %r" % (fullname,),
                                 name=fullname)
        return os.path.join(self.archive, found[0])

    def is_package(self, fullname):
        found = self._find(fullname)
        if found is None:
            raise ZipImportError("can't find module %r" % (fullname,),
                                 name=fullname)
        return found[1]

    def get_data(self, pathname):
        """Return raw bytes for *pathname* (``<archive>/<relpath>``)."""
        path = os.fsdecode(pathname)
        rel = path
        # Strip the archive prefix (with or without the separator) so callers
        # can pass either an absolute ``<archive>/x`` path or a bare ``x``.
        if rel.startswith(self.archive):
            rel = rel[len(self.archive):]
            rel = rel.lstrip(os.sep).lstrip("/")
        rel = rel.replace(os.sep, "/")
        try:
            return self._read(rel)
        except KeyError:
            raise OSError("zipimport: file not found in %r: %r"
                          % (self.archive, pathname))

    def create_module(self, spec):
        # Default module creation (CPython returns None here too).
        return None

    def exec_module(self, module):
        code = self.get_code(module.__name__)
        exec(code, module.__dict__)

    def invalidate_caches(self):
        _zip_directory_cache.pop(self.archive, None)
