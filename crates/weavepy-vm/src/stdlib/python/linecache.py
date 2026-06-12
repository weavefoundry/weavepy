"""WeavePy `linecache` — line-by-line source cache.

Used by `traceback`, `inspect.getsource`, and warning machinery to
turn (filename, lineno) into a source line for display. Mirrors
CPython's cache model: `cache` maps filename → 4-tuple
`(size, mtime, lines, fullname)`, with *lazy* entries stored as
1-tuples `(get_source,)` until first use (PEP 302 loaders).
"""

import os
import sys


# Public, exactly like CPython — tests poke `linecache.cache` directly.
cache = {}


__all__ = [
    "getline",
    "getlines",
    "clearcache",
    "checkcache",
    "lazycache",
    "updatecache",
]


def clearcache():
    cache.clear()


def getline(filename, lineno, module_globals=None):
    lines = getlines(filename, module_globals)
    if 1 <= lineno <= len(lines):
        return lines[lineno - 1]
    return ""


def _getline_from_code(code, lineno):
    """Source line for a code object whose file isn't on disk
    (e.g. `<stdin>`); CPython keeps a code-object-keyed side cache."""
    lines = _getlines_from_code(code)
    if 1 <= lineno <= len(lines):
        return lines[lineno - 1]
    return ""


# CPython keys this side cache by (co_filename, co_qualname,
# co_firstlineno) rather than the code object itself: the frame being
# rendered holds a *nested* code object (a method compiled inside the
# registered module code), and value-keying lets registration walk
# `co_consts` once and cover every nested function.
_interactive_cache = {}


def _make_key(code):
    return (code.co_filename, code.co_qualname, code.co_firstlineno)


def _register_code(code, string, name):
    entry = (
        len(string),
        None,
        [line + "\n" for line in string.splitlines()],
        name,
    )
    stack = [code]
    while stack:
        code = stack.pop()
        for const in code.co_consts:
            if isinstance(const, type(code)):
                stack.append(const)
        _interactive_cache[_make_key(code)] = entry


def _getlines_from_code(code):
    entry = _interactive_cache.get(_make_key(code))
    if entry is not None and len(entry) != 1:
        return entry[2]
    return []


def getlines(filename, module_globals=None):
    if filename in cache:
        entry = cache[filename]
        if len(entry) != 1:
            return entry[2]
    try:
        return updatecache(filename, module_globals) or []
    except (MemoryError, KeyboardInterrupt):
        raise
    except Exception:
        return []


def checkcache(filename=None):
    if filename is None:
        filenames = list(cache.keys())
    elif filename in cache:
        filenames = [filename]
    else:
        return
    for f in filenames:
        entry = cache.get(f)
        if entry is None or len(entry) == 1:
            # Lazy entries have no stat to validate.
            continue
        size, mtime, lines, name = entry
        if mtime is None:
            continue
        try:
            stat = os.stat(name)
        except OSError:
            cache.pop(f, None)
            continue
        if size != stat.st_size or mtime != stat.st_mtime:
            cache.pop(f, None)


def updatecache(filename, module_globals=None):
    entry = cache.pop(filename, None)
    # Lazy 1-tuple — materialise through the loader's get_source.
    if entry is not None and len(entry) == 1:
        try:
            data = entry[0]()
        except (ImportError, OSError):
            data = None
        if data is not None:
            lines = data.splitlines(keepends=True)
            cache[filename] = (len(data), None, lines, filename)
            return lines
    # WeavePy frozen stdlib modules carry `<frozen NAME>` filenames;
    # their source is recoverable through `_imp.find_frozen`.
    if filename.startswith("<frozen ") and filename.endswith(">"):
        modname = filename[8:-1]
        try:
            import _imp
            found = _imp.find_frozen(modname)
        except Exception:
            found = None
        if found is not None:
            src = found[0]
            if isinstance(src, bytes):
                src = src.decode("utf-8", "replace")
            lines = src.splitlines(keepends=True)
            cache[filename] = (len(lines), None, lines, filename)
            return lines
    name = filename
    # Try direct file system access.
    try:
        stat = os.stat(name)
    except OSError:
        # One more chance: a loader from module_globals (CPython tries
        # this when the file isn't directly readable).
        if lazycache(filename, module_globals):
            return updatecache(filename, module_globals)
        return []
    try:
        with open(name, "rb") as f:
            raw = f.read()
        data = _decode_source(raw)
    except (OSError, UnicodeDecodeError, LookupError):
        return []
    lines = data.splitlines(keepends=True)
    if lines and not lines[-1].endswith("\n"):
        lines[-1] += "\n"
    cache[filename] = (stat.st_size, stat.st_mtime, lines, name)
    return lines


def _coding_cookie(line):
    """PEP 263 cookie in a comment line (bytes), or None.

    Hand-rolled equivalent of tokenize's
    `^[ \\t\\f]*#.*?coding[:=][ \\t]*([-_.a-zA-Z0-9]+)` so linecache
    doesn't have to import `re` mid-traceback.
    """
    i = 0
    while i < len(line) and line[i : i + 1] in (b" ", b"\t", b"\x0c"):
        i += 1
    if line[i : i + 1] != b"#":
        return None
    pos = line.find(b"coding", i)
    if pos < 0:
        return None
    j = pos + 6
    if line[j : j + 1] not in (b":", b"="):
        return None
    j += 1
    while line[j : j + 1] in (b" ", b"\t"):
        j += 1
    start = j
    while j < len(line) and chr(line[j]) in (
        "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_."
    ):
        j += 1
    if j == start:
        return None
    return line[start:j].decode("ascii")


def _decode_source(raw):
    """Decode source bytes the way `tokenize.open` would: UTF-8 BOM,
    then a PEP 263 coding cookie on line 1 or 2, defaulting to UTF-8."""
    if raw.startswith(b"\xef\xbb\xbf"):
        return raw[3:].decode("utf-8")
    for line in raw.split(b"\n", 2)[:2]:
        encoding = _coding_cookie(line)
        if encoding is not None:
            return raw.decode(encoding)
    return raw.decode("utf-8")


def lazycache(filename, module_globals):
    """Seed the cache with a deferred get_source for `filename`.

    Returns True if a lazy entry was (or already is) installed.
    """
    if filename in cache:
        return len(cache[filename]) == 1
    if not filename or (filename.startswith("<") and filename.endswith(">")):
        return False
    if module_globals is None:
        return False
    name = module_globals.get("__name__")
    loader = module_globals.get("__loader__")
    spec = module_globals.get("__spec__")
    if loader is None and spec is not None:
        loader = getattr(spec, "loader", None)
    get_source = getattr(loader, "get_source", None)
    if get_source is None:
        # WeavePy's import machinery doesn't install `__loader__` on
        # disk-loaded modules yet; synthesize the loader CPython would
        # have used from `__file__`.
        file = module_globals.get("__file__")
        if name and file:
            try:
                from importlib.machinery import SourceFileLoader
            except ImportError:
                return False
            get_source = SourceFileLoader(name, file).get_source
    if name and get_source:
        def get_lines(name=name, *args, **kwargs):
            return get_source(name, *args, **kwargs)
        cache[filename] = (get_lines,)
        return True
    return False
