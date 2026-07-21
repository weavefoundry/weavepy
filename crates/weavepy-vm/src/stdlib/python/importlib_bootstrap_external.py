"""``importlib._bootstrap_external`` — WeavePy façade.

In CPython this frozen module defines the filesystem loaders, which
``importlib.machinery`` then re-exports. WeavePy defines the loaders in
``importlib.machinery`` directly, so this module is the alias in the
other direction — stdlib code (e.g. ``pydoc.locate``-adjacent paths)
imports the names from here.
"""

from importlib.machinery import (
    SOURCE_SUFFIXES,
    BYTECODE_SUFFIXES,
    EXTENSION_SUFFIXES,
    SourceFileLoader,
    SourcelessFileLoader,
    ExtensionFileLoader,
)

__all__ = [
    'SOURCE_SUFFIXES',
    'BYTECODE_SUFFIXES',
    'EXTENSION_SUFFIXES',
    'SourceFileLoader',
    'SourcelessFileLoader',
    'ExtensionFileLoader',
]

import sys as _sys

# Path-manipulation primitives the verbatim `zipimport` builds on.
path_sep = '\\' if _sys.platform == 'win32' else '/'
path_separators = '\\/' if _sys.platform == 'win32' else '/'
_pathseps_with_colon = set(f':{s}' for s in path_separators)


def _pack_uint32(x):
    """Convert a 32-bit integer to little-endian."""
    return (int(x) & 0xFFFFFFFF).to_bytes(4, 'little')


def _unpack_uint64(data):
    """Convert 8 bytes in little-endian to an integer."""
    assert len(data) == 8
    return int.from_bytes(data, 'little')


def _unpack_uint32(data):
    """Convert 4 bytes in little-endian to an integer."""
    assert len(data) == 4
    return int.from_bytes(data, 'little')


def _unpack_uint16(data):
    """Convert 2 bytes in little-endian to an integer."""
    assert len(data) == 2
    return int.from_bytes(data, 'little')


def _path_join(*path_parts):
    """Replacement for os.path.join(): drops empty parts and trailing
    separators (CPython's POSIX variant — zipimporter's prefix
    computation relies on both). Adequate on Windows too for the
    archive-relative joins zipimport performs.
    """
    return path_sep.join([part.rstrip(path_separators)
                          for part in path_parts if part])


def _path_split(path):
    """Replacement for os.path.split()."""
    i = max(path.rfind(p) for p in path_separators)
    if i < 0:
        return '', path
    return path[:i], path[i + 1:]


def _path_stat(path):
    """Stat the path without consulting any caches."""
    import os
    return os.stat(path)


def _fix_up_module(ns, name, pathname, cpathname=None):
    """Populate a module namespace's loader/spec/file attributes the way
    the import system would (CPython verbatim, module-shim path)."""
    loader = ns.get('__loader__')
    spec = ns.get('__spec__')
    if not loader:
        if spec:
            loader = spec.loader
        elif pathname == cpathname:
            from importlib.machinery import SourcelessFileLoader
            loader = SourcelessFileLoader(name, pathname)
        else:
            from importlib.machinery import SourceFileLoader
            loader = SourceFileLoader(name, pathname)
    if not spec:
        from importlib.util import spec_from_file_location
        spec = spec_from_file_location(name, pathname, loader=loader)
        if cpathname:
            spec.cached = cpathname
    try:
        ns['__spec__'] = spec
        ns['__loader__'] = loader
        ns['__file__'] = pathname
        ns['__cached__'] = cpathname
    except Exception:
        pass


class _LoaderBasics:
    """Base class of common code needed by SourceLoader and zipimporter."""

    def is_package(self, fullname):
        """Concrete implementation of InspectLoader.is_package by checking if
        the path returned by get_filename has a filename of '__init__.py'."""
        import os.path
        filename = os.path.split(self.get_filename(fullname))[1]
        filename_base = filename.rsplit('.', 1)[0]
        tail_name = fullname.rpartition('.')[2]
        return filename_base == '__init__' and tail_name != '__init__'

    def create_module(self, spec):
        """Use default semantics for module creation."""

    def exec_module(self, module):
        """Execute the module."""
        code = self.get_code(module.__name__)
        if code is None:
            raise ImportError(f'cannot load module {module.__name__!r} when '
                              'get_code() returns None')
        exec(code, module.__dict__)

    def load_module(self, fullname):
        """This method is deprecated."""
        import importlib._bootstrap as _bootstrap
        return _bootstrap._load_module_shim(self, fullname)


def _code_to_timestamp_pyc(code, mtime=0, source_size=0):
    "Produce the data for a timestamp-based pyc (CPython verbatim)."
    import marshal
    from importlib.util import MAGIC_NUMBER
    data = bytearray(MAGIC_NUMBER)
    data.extend(_pack_uint32(0))
    data.extend(_pack_uint32(mtime))
    data.extend(_pack_uint32(source_size))
    data.extend(marshal.dumps(code))
    return data


def _code_to_hash_pyc(code, source_hash, checked=True):
    "Produce the data for a hash-based pyc (CPython verbatim)."
    import marshal
    from importlib.util import MAGIC_NUMBER
    data = bytearray(MAGIC_NUMBER)
    flags = 0b1 | checked << 1
    data.extend(_pack_uint32(flags))
    assert len(source_hash) == 8
    data.extend(source_hash)
    data.extend(marshal.dumps(code))
    return data


def _calc_mode(path):
    """Calculate the mode permissions for a bytecode file."""
    import os
    try:
        mode = os.stat(path).st_mode
    except OSError:
        mode = 0o666
    # We always ensure write access so we can update cached files
    # later even when the source files are read-only on Windows (#6074)
    mode |= 0o200
    return mode


def _write_atomic(path, data, mode=0o666):
    """Best-effort function to write data to a path atomically.
    Be prepared to handle a FileExistsError if concurrent writing of the
    temporary file is attempted."""
    import os
    path_tmp = '{}.{}'.format(path, id(path))
    fd = os.open(path_tmp, os.O_EXCL | os.O_CREAT | os.O_WRONLY, mode & 0o666)
    try:
        try:
            os.write(fd, bytes(data))
        finally:
            os.close(fd)
        os.replace(path_tmp, path)
    except OSError:
        try:
            os.unlink(path_tmp)
        except OSError:
            pass
        raise


from importlib.machinery import MAGIC_NUMBER

_RAW_MAGIC_NUMBER = int.from_bytes(MAGIC_NUMBER, 'little')


def _classify_pyc(data, name, exc_details):
    """Perform basic validity checking of a pyc header and return the flags
    field, which determines how the pyc should be further validated against
    the source (CPython verbatim).
    """
    from importlib.util import MAGIC_NUMBER
    magic = data[:4]
    if magic != MAGIC_NUMBER:
        message = f'bad magic number in {name!r}: {magic!r}'
        raise ImportError(message, **exc_details)
    if len(data) < 16:
        message = f'reached EOF while reading pyc header of {name!r}'
        raise EOFError(message)
    flags = int.from_bytes(data[4:8], 'little')
    # Only the first two flags are defined.
    if flags & ~0b11:
        message = f'invalid flags {flags!r} in {name!r}'
        raise ImportError(message, **exc_details)
    return flags


def _validate_timestamp_pyc(data, source_mtime, source_size, name,
                            exc_details):
    """Validate a pyc against the source last-modified time
    (CPython verbatim)."""
    if int.from_bytes(data[8:12], 'little') != (source_mtime & 0xFFFFFFFF):
        message = f'bytecode is stale for {name!r}'
        raise ImportError(message, **exc_details)
    if (source_size is not None and
            int.from_bytes(data[12:16], 'little') != (source_size & 0xFFFFFFFF)):
        raise ImportError(f'bytecode is stale for {name!r}', **exc_details)


def _validate_hash_pyc(data, source_hash, name, exc_details):
    """Validate a hash-based pyc by checking the real source hash against
    the one in the pyc header (CPython verbatim)."""
    if data[8:16] != source_hash:
        raise ImportError(
            f'hash in bytecode doesn\'t match hash of source {name!r}',
            **exc_details,
        )


def _get_sourcefile(bytecode_path):
    """Convert a bytecode file path to a source path (if possible).

    Verbatim CPython ``importlib/_bootstrap_external.py`` logic
    (``test_import`` imports this helper directly).
    """
    if len(bytecode_path) == 0:
        return None
    rest, _, extension = bytecode_path.rpartition('.')
    if not rest or extension.lower()[-3:-1] != 'py':
        return bytecode_path
    try:
        from importlib.util import source_from_cache
        source_path = source_from_cache(bytecode_path)
    except (NotImplementedError, ValueError, ImportError):
        source_path = bytecode_path[:-1]
    import os.path
    return source_path if os.path.isfile(source_path) else bytecode_path
