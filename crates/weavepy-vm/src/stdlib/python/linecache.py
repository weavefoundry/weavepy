"""WeavePy `linecache` — line-by-line source cache.

Used by `traceback`, `inspect.getsource`, and warning machinery to
turn (filename, lineno) into a source line for display. The cache is
re-read with `checkcache()`; for files loaded via `importlib`, the
module's loader can install a custom getter via `lazycache`.
"""

import os
import sys


_cache = {}
_lazycache = {}


__all__ = [
    "getline",
    "getlines",
    "clearcache",
    "checkcache",
    "lazycache",
    "updatecache",
]


def clearcache():
    _cache.clear()


def getline(filename, lineno, module_globals=None):
    lines = getlines(filename, module_globals)
    if 1 <= lineno <= len(lines):
        return lines[lineno - 1]
    return ""


def getlines(filename, module_globals=None):
    if filename in _cache:
        entry = _cache[filename]
        if entry is None:
            return []
        return entry[2]
    try:
        return updatecache(filename, module_globals) or []
    except (MemoryError, KeyboardInterrupt):
        raise
    except Exception:
        return []


def checkcache(filename=None):
    if filename is None:
        filenames = list(_cache.keys())
    else:
        if filename in _cache:
            filenames = [filename]
        else:
            return
    for f in filenames:
        entry = _cache.get(f)
        if entry is None:
            continue
        size, mtime, lines, name = entry
        if mtime is None:
            continue
        try:
            stat = os.stat(name)
        except OSError:
            _cache.pop(f, None)
            continue
        if size != stat.st_size or mtime != stat.st_mtime:
            _cache.pop(f, None)


def updatecache(filename, module_globals=None):
    if filename in _cache:
        if _cache[filename] is None:
            return []
        _cache.pop(filename)
    # Try lazycache fallback first.
    if filename in _lazycache:
        getter, mg = _lazycache.pop(filename)
        lines = getter()
        if lines is not None:
            if isinstance(lines, str):
                lines = lines.splitlines(keepends=True)
            _cache[filename] = (len(lines), None, lines, filename)
            return lines
    name = filename
    # Try direct file system access.
    try:
        stat = os.stat(name)
    except OSError:
        return []
    try:
        with open(name, encoding="utf-8") as f:
            data = f.read()
    except (OSError, UnicodeDecodeError):
        return []
    lines = data.splitlines(keepends=True)
    if lines and not lines[-1].endswith("\n"):
        lines[-1] += "\n"
    _cache[filename] = (stat.st_size, stat.st_mtime, lines, name)
    return lines


def lazycache(filename, module_globals=None):
    if filename in _cache:
        return True
    if module_globals is None:
        return False
    loader = module_globals.get("__loader__")
    if loader is None:
        return False
    get_source = getattr(loader, "get_source", None)
    if get_source is None:
        return False
    name = module_globals.get("__name__")

    def _load():
        try:
            return get_source(name)
        except Exception:
            return None

    _lazycache[filename] = (_load, module_globals)
    return True
