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
