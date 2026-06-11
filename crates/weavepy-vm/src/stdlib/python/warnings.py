"""WeavePy `warnings` — issue warnings and control how they're shown.

The implementation tracks CPython's user-visible API: `warn`,
`warn_explicit`, `simplefilter`, `filterwarnings`, `resetwarnings`,
`catch_warnings`, and `formatwarning`. Filters live in a global list
and are evaluated in order — the first match wins, matching CPython
semantics.
"""

import sys
import linecache


__all__ = [
    "warn",
    "warn_explicit",
    "showwarning",
    "formatwarning",
    "filterwarnings",
    "simplefilter",
    "resetwarnings",
    "catch_warnings",
    "WarningMessage",
    "defaultaction",
    "filters",
    "onceregistry",
]


# Filter spec: (action, message_regex_or_None, category, module_regex_or_None, lineno)
filters = []
onceregistry = {}
defaultaction = "default"
_filters_version = 0


class WarningMessage:
    """A captured warning when running under `catch_warnings(record=True)`."""

    _WARNING_DETAILS = (
        "message",
        "category",
        "filename",
        "lineno",
        "file",
        "line",
        "source",
    )

    def __init__(self, message, category, filename, lineno, file=None, line=None, source=None):
        self.message = message
        self.category = category
        self.filename = filename
        self.lineno = lineno
        self.file = file
        self.line = line
        self.source = source

    def __str__(self):
        return f"{{message : {self.message!r}, category : {self.category.__name__!r}, filename : {self.filename!r}, lineno : {self.lineno}, line : {self.line!r}}}"


def _filters_mutated():
    global _filters_version
    _filters_version += 1


def filterwarnings(action, message="", category=Warning, module="", lineno=0, append=False):
    """Insert an entry into `filters`."""
    if action not in ("error", "ignore", "always", "default", "module", "once"):
        raise ValueError(f"invalid action: {action!r}")
    if not isinstance(lineno, int) or lineno < 0:
        raise ValueError("lineno must be an int >= 0")
    # Compile message and module patterns lazily (re is optional).
    item = (action, message, category, module, lineno)
    if append:
        filters.append(item)
    else:
        try:
            filters.remove(item)
        except ValueError:
            pass
        filters.insert(0, item)
    _filters_mutated()


def simplefilter(action, category=Warning, lineno=0, append=False):
    if action not in ("error", "ignore", "always", "default", "module", "once"):
        raise ValueError(f"invalid action: {action!r}")
    item = (action, None, category, None, lineno)
    if append:
        filters.append(item)
    else:
        try:
            filters.remove(item)
        except ValueError:
            pass
        filters.insert(0, item)
    _filters_mutated()


def resetwarnings():
    filters[:] = []
    _filters_mutated()


def _import_re():
    try:
        import re
        return re
    except Exception:
        return None


def _match(pattern, text):
    if pattern is None or pattern == "":
        return True
    if isinstance(pattern, str):
        re_mod = _import_re()
        if re_mod is None:
            return text.startswith(pattern)
        try:
            return bool(re_mod.match(pattern, text))
        except Exception:
            return text.startswith(pattern)
    # Already-compiled pattern.
    return bool(pattern.match(text))


def _get_frame(depth):
    if not hasattr(sys, "_getframe"):
        return None
    try:
        return sys._getframe(depth + 1)
    except ValueError:
        return None


def warn(message, category=UserWarning, stacklevel=1, source=None):
    if isinstance(message, Warning):
        category = type(message)
    elif not (isinstance(category, type) and issubclass(category, Warning)):
        category = UserWarning
    frame = _get_frame(stacklevel)
    if frame is not None:
        globals_ = frame.f_globals
        lineno = frame.f_lineno
    else:
        globals_ = sys.__dict__
        lineno = 1
    registry = globals_.setdefault("__warningregistry__", {})
    module_name = globals_.get("__name__", "<unknown>")
    filename = globals_.get("__file__", "<unknown>")
    text = str(message)
    warn_explicit(message, category, filename, lineno, module=module_name,
                  registry=registry, module_globals=globals_, source=source)


def _warn_unawaited_coroutine(coro):
    """Called by the VM when a coroutine is finalized without ever
    being awaited (CPython's identically-named hook in Lib/warnings.py).
    Appends the cr_origin creation traceback when origin tracking is on.
    """
    msg_lines = [
        f"coroutine '{coro.__qualname__}' was never awaited\n"
    ]
    if getattr(coro, "cr_origin", None) is not None:
        import linecache
        import traceback

        def extract():
            for filename, lineno, funcname in reversed(coro.cr_origin):
                line = linecache.getline(filename, lineno).strip()
                yield (filename, lineno, funcname, line)

        msg_lines.append("Coroutine created at (most recent call last)\n")
        msg_lines += traceback.format_list(list(extract()))
    msg = "".join(msg_lines).rstrip("\n")
    warn(msg, category=RuntimeWarning, stacklevel=2, source=coro)


def warn_explicit(message, category, filename, lineno, module=None,
                  registry=None, module_globals=None, source=None):
    if registry is None:
        registry = {}
    if module is None:
        module = filename or "<unknown>"
        if module[-3:].lower() == ".py":
            module = module[:-3]
    if isinstance(message, Warning):
        text = str(message)
        category = type(message)
    else:
        text = str(message)
        if not isinstance(category, type) or not issubclass(category, Warning):
            category = UserWarning
        message = category(text)
    key = (text, category, lineno)
    if registry.get(key):
        return
    action = defaultaction
    matched_filter = None
    for f_action, f_msg, f_cat, f_mod, f_lineno in filters:
        if (
            (f_msg is None or _match(f_msg, text))
            and issubclass(category, f_cat)
            and (f_mod is None or _match(f_mod, module))
            and (f_lineno == 0 or f_lineno == lineno)
        ):
            action = f_action
            matched_filter = (f_msg, f_cat, f_mod, f_lineno)
            break
    if action == "error":
        raise message
    if action == "ignore":
        return
    if action == "once":
        oncekey = (text, category)
        if onceregistry.get(oncekey):
            return
        onceregistry[oncekey] = 1
    elif action == "always":
        pass
    elif action == "module":
        altkey = (text, category, 0)
        if registry.get(altkey):
            return
        registry[altkey] = 1
    elif action == "default":
        registry[key] = 1
    showwarning(message, category, filename, lineno, source=source)


def formatwarning(message, category, filename, lineno, line=None):
    out = f"{filename}:{lineno}: {category.__name__}: {message}\n"
    if line is None:
        try:
            line = linecache.getline(filename, lineno)
        except Exception:
            line = ""
    line = (line or "").strip()
    if line:
        out += "  " + line + "\n"
    return out


def showwarning(message, category, filename, lineno, file=None, line=None, source=None):
    if file is None:
        file = sys.stderr
    if file is None:
        return
    try:
        file.write(formatwarning(message, category, filename, lineno, line))
    except Exception:
        pass


class catch_warnings:
    """Context manager that saves and restores the warning state."""

    def __init__(self, *, record=False, module=None, action=None,
                 category=Warning, lineno=0, append=False):
        self._record = record
        self._module = module if module is not None else sys.modules.get("warnings")
        self._action = action
        self._category = category
        self._lineno = lineno
        self._append = append
        self._entered = False
        self._saved_filters = None
        self._saved_showwarning = None
        self._saved_default = None
        self._log = None

    def __enter__(self):
        if self._entered:
            raise RuntimeError("cannot reuse catch_warnings instance")
        self._entered = True
        self._saved_filters = list(filters)
        self._saved_default = defaultaction
        global showwarning, _filters_version
        self._saved_showwarning = showwarning
        if self._record:
            self._log = []

            def log(message, category, filename, lineno, file=None, line=None, source=None):
                self._log.append(
                    WarningMessage(message, category, filename, lineno, file, line, source)
                )

            globals()["showwarning"] = log
        _filters_mutated()
        if self._action is not None:
            simplefilter(self._action, self._category, self._lineno, self._append)
        return self._log if self._record else None

    def __exit__(self, *exc):
        global defaultaction
        filters[:] = self._saved_filters
        globals()["showwarning"] = self._saved_showwarning
        defaultaction = self._saved_default
        _filters_mutated()
        return False


_DEPRECATED_MSG = "{name!r} is deprecated and slated for removal in Python {remove}"


def _deprecated(name, message=_DEPRECATED_MSG, *, remove, _version=sys.version_info):
    """Warn that *name* is deprecated or should be removed.

    RuntimeError is raised if *remove* specifies a major/minor tuple older than
    the current Python version or the same version but past the alpha.

    The *message* argument is formatted with *name* and *remove* as a Python
    version tuple (e.g. (3, 11)).

    """
    remove_formatted = f"{remove[0]}.{remove[1]}"
    if (_version[:2] > remove) or (_version[:2] == remove and _version[3] != "alpha"):
        msg = f"{name!r} was slated for removal after Python {remove_formatted} alpha"
        raise RuntimeError(msg)
    else:
        msg = message.format(name=name, remove=remove_formatted)
        warn(msg, DeprecationWarning, stacklevel=3)


# Install a sane default filter set on import.
simplefilter("default")
