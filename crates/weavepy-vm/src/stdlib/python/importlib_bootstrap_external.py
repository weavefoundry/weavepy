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
