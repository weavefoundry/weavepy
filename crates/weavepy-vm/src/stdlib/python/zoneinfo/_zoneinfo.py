"""``zoneinfo._zoneinfo`` — the pure-Python ``ZoneInfo`` implementation.

CPython ships the reference implementation as ``Lib/zoneinfo/_zoneinfo.py``
and ``zoneinfo/__init__.py`` falls back to it when the ``_zoneinfo`` C
accelerator is unavailable. WeavePy inlines the implementation into the
package ``__init__`` instead, so this submodule just re-exports the pure
class under CPython's module path. It must exist as a real module:
pandas' Cython ``tslibs.timezones`` imports ``zoneinfo._zoneinfo``
directly at extension-init time.
"""

from zoneinfo import _PurePythonZoneInfo as ZoneInfo

__all__ = ["ZoneInfo"]
