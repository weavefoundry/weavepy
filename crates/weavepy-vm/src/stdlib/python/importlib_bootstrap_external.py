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


def _pack_uint32(x):
    """Convert a 32-bit integer to little-endian."""
    return (int(x) & 0xFFFFFFFF).to_bytes(4, 'little')


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
