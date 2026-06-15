"""WeavePy `logging` — small but faithful logging library.

Implements CPython's hierarchical-logger model. The cooperative
parts of the standard library — `Logger`, `Handler`, `Formatter`,
`StreamHandler`, `FileHandler`, `NullHandler`, `basicConfig`,
`getLogger`, `disable`, `addLevelName` — all behave the way user
code expects.
"""

import sys
import time as _time
import threading as _threading


__all__ = [
    "Logger",
    "Handler",
    "Formatter",
    "StreamHandler",
    "FileHandler",
    "NullHandler",
    "BufferingHandler",
    "MemoryHandler",
    "LogRecord",
    "Filter",
    "PercentStyle",
    "BraceStyle",
    "basicConfig",
    "getLogger",
    "getLevelName",
    "addLevelName",
    "disable",
    "shutdown",
    "captureWarnings",
    "makeLogRecord",
    "log",
    "debug",
    "info",
    "warning",
    "error",
    "critical",
    "exception",
    "fatal",
    "warn",
    "NOTSET",
    "DEBUG",
    "INFO",
    "WARNING",
    "WARN",
    "ERROR",
    "CRITICAL",
    "FATAL",
    "BASIC_FORMAT",
]


CRITICAL = 50
FATAL = CRITICAL
ERROR = 40
WARNING = 30
WARN = WARNING
INFO = 20
DEBUG = 10
NOTSET = 0


_levelToName = {
    CRITICAL: "CRITICAL",
    ERROR: "ERROR",
    WARNING: "WARNING",
    INFO: "INFO",
    DEBUG: "DEBUG",
    NOTSET: "NOTSET",
}

_nameToLevel = {v: k for k, v in _levelToName.items()}

BASIC_FORMAT = "%(levelname)s:%(name)s:%(message)s"


def getLevelName(level):
    if isinstance(level, str):
        return _nameToLevel.get(level, "Level " + level)
    return _levelToName.get(level, "Level " + str(level))


def addLevelName(level, name):
    _levelToName[level] = name
    _nameToLevel[name] = level


def _check_level(level):
    if isinstance(level, int):
        return level
    if isinstance(level, str):
        if level not in _nameToLevel:
            raise ValueError(f"Unknown level: {level!r}")
        return _nameToLevel[level]
    raise TypeError(f"Level not an int or str: {level!r}")


_lock = _threading.RLock()


class LogRecord:
    """A single log event."""

    def __init__(self, name, level, pathname, lineno, msg, args, exc_info, func=None):
        ct = _time.time()
        self.name = name
        self.msg = msg
        if args and len(args) == 1 and isinstance(args[0], dict):
            self.args = args[0]
        else:
            self.args = args
        self.levelname = getLevelName(level)
        self.levelno = level
        self.pathname = pathname
        try:
            self.filename = pathname.rsplit("/", 1)[-1]
            self.module = self.filename.split(".", 1)[0]
        except Exception:
            self.filename = pathname
            self.module = "Unknown module"
        self.exc_info = exc_info
        self.exc_text = None
        self.stack_info = None
        self.lineno = lineno
        self.funcName = func
        self.created = ct
        self.msecs = int((ct - int(ct)) * 1000)
        self.relativeCreated = (ct - _startTime) * 1000
        self.thread = 1
        self.threadName = "MainThread"
        self.process = 0
        self.processName = "MainProcess"

    def getMessage(self):
        msg = str(self.msg)
        if self.args:
            try:
                msg = msg % self.args
            except Exception:
                pass
        return msg

    def __repr__(self):
        return f"<LogRecord: {self.name}, {self.levelno}, {self.pathname}, {self.lineno}>"


_startTime = _time.time()


def makeLogRecord(d):
    rec = LogRecord(None, None, "", 0, "", None, None)
    rec.__dict__.update(d)
    return rec


# ---------------- Formatter ---------------- #

class PercentStyle:
    default_format = "%(message)s"
    asctime_format = "%(asctime)s"
    asctime_search = "%(asctime)"
    validation_pattern = None

    def __init__(self, fmt, defaults=None):
        self._fmt = fmt or self.default_format
        self._defaults = defaults

    def usesTime(self):
        return self._fmt.find(self.asctime_search) >= 0

    def format(self, record):
        values = dict(record.__dict__)
        if self._defaults:
            for k, v in self._defaults.items():
                values.setdefault(k, v)
        return self._fmt % values


class BraceStyle(PercentStyle):
    default_format = "{message}"
    asctime_format = "{asctime}"
    asctime_search = "{asctime"

    def format(self, record):
        values = dict(record.__dict__)
        if self._defaults:
            for k, v in self._defaults.items():
                values.setdefault(k, v)
        try:
            return self._fmt.format(**values)
        except KeyError as exc:
            return self._fmt + f" [missing field {exc}]"


_STYLES = {
    "%": PercentStyle,
    "{": BraceStyle,
}


class Formatter:
    def __init__(self, fmt=None, datefmt=None, style="%", validate=True, *,
                 defaults=None):
        if style not in _STYLES:
            raise ValueError("Style must be one of: " + ", ".join(_STYLES))
        self._style = _STYLES[style](fmt, defaults=defaults)
        self._fmt = self._style._fmt
        self.datefmt = datefmt

    def formatTime(self, record, datefmt=None):
        ct = _time.localtime(record.created)
        if datefmt:
            return _time.strftime(datefmt, ct)
        return _time.strftime("%Y-%m-%d %H:%M:%S", ct) + f",{record.msecs:03d}"

    def formatException(self, ei):
        import traceback
        sio = []
        try:
            sio.extend(traceback.format_exception(ei[0], ei[1], ei[2]))
        except Exception:
            sio.append("(could not format exception)")
        return "".join(sio).rstrip()

    def formatStack(self, stack_info):
        return stack_info

    def format(self, record):
        record.message = record.getMessage()
        if self._style.usesTime():
            record.asctime = self.formatTime(record, self.datefmt)
        s = self._style.format(record)
        if record.exc_info:
            if not record.exc_text:
                record.exc_text = self.formatException(record.exc_info)
        if record.exc_text:
            if s[-1:] != "\n":
                s += "\n"
            s += record.exc_text
        if record.stack_info:
            if s[-1:] != "\n":
                s += "\n"
            s += self.formatStack(record.stack_info)
        return s


_defaultFormatter = Formatter()


class BufferingFormatter:
    """A formatter suitable for formatting a number of records."""

    def __init__(self, linefmt=None):
        if linefmt:
            self.linefmt = linefmt
        else:
            self.linefmt = _defaultFormatter

    def formatHeader(self, records):
        return ""

    def formatFooter(self, records):
        return ""

    def format(self, records):
        rv = ""
        if len(records) > 0:
            rv = rv + self.formatHeader(records)
            for record in records:
                rv = rv + self.linefmt.format(record)
            rv = rv + self.formatFooter(records)
        return rv


# ---------------- Filter ---------------- #

class Filter:
    def __init__(self, name=""):
        self.name = name
        self.nlen = len(name)

    def filter(self, record):
        if self.nlen == 0:
            return True
        if self.name == record.name:
            return True
        if record.name.find(self.name, 0, self.nlen) != 0:
            return False
        return record.name[self.nlen:self.nlen + 1] == "."


class Filterer:
    def __init__(self):
        self.filters = []

    def addFilter(self, flt):
        if flt not in self.filters:
            self.filters.append(flt)

    def removeFilter(self, flt):
        if flt in self.filters:
            self.filters.remove(flt)

    def filter(self, record):
        for f in self.filters:
            if hasattr(f, "filter"):
                result = f.filter(record)
            else:
                result = f(record)
            if not result:
                return False
            if isinstance(result, LogRecord):
                record = result
        return True


# ---------------- Handlers ---------------- #

class Handler(Filterer):
    def __init__(self, level=NOTSET):
        Filterer.__init__(self)
        self.level = _check_level(level)
        self.formatter = None
        self._name = None
        self.lock = _threading.RLock()

    def get_name(self):
        return self._name

    def set_name(self, name):
        with _lock:
            if self._name in _handlers:
                _handlers.pop(self._name)
            self._name = name
            if name:
                _handlers[name] = self

    name = property(get_name, set_name)

    def setLevel(self, level):
        self.level = _check_level(level)

    def setFormatter(self, fmt):
        self.formatter = fmt

    def format(self, record):
        if self.formatter is not None:
            return self.formatter.format(record)
        return _defaultFormatter.format(record)

    def emit(self, record):
        raise NotImplementedError

    def handle(self, record):
        rv = self.filter(record)
        if rv:
            with self.lock:
                self.emit(record)
        return rv

    def flush(self):
        pass

    def close(self):
        with _lock:
            if self._name and self._name in _handlers:
                _handlers.pop(self._name, None)

    def handleError(self, record):
        if logging.raiseExceptions:
            import traceback
            traceback.print_exc(file=sys.stderr)


class StreamHandler(Handler):
    terminator = "\n"

    def __init__(self, stream=None):
        Handler.__init__(self)
        if stream is None:
            stream = sys.stderr
        self.stream = stream

    def flush(self):
        with self.lock:
            if self.stream and hasattr(self.stream, "flush"):
                self.stream.flush()

    def emit(self, record):
        try:
            msg = self.format(record)
            self.stream.write(msg + self.terminator)
            self.flush()
        except RecursionError:
            raise
        except Exception:
            self.handleError(record)

    def setStream(self, stream):
        with self.lock:
            if stream is self.stream:
                return None
            result = self.stream
            self.stream = stream
        return result


class FileHandler(StreamHandler):
    def __init__(self, filename, mode="a", encoding=None, delay=False, errors=None):
        self.baseFilename = filename
        self.mode = mode
        self.encoding = encoding
        self.errors = errors
        self.delay = delay
        if delay:
            Handler.__init__(self)
            self.stream = None
        else:
            stream = self._open()
            StreamHandler.__init__(self, stream)

    def _open(self):
        return open(self.baseFilename, self.mode, encoding=self.encoding,
                    errors=self.errors)

    def emit(self, record):
        if self.stream is None:
            self.stream = self._open()
        StreamHandler.emit(self, record)

    def close(self):
        with self.lock:
            try:
                if self.stream:
                    self.stream.flush()
                    self.stream.close()
                    self.stream = None
            finally:
                StreamHandler.close(self)


class NullHandler(Handler):
    def emit(self, record):
        pass

    def handle(self, record):
        pass


class BufferingHandler(Handler):
    def __init__(self, capacity):
        Handler.__init__(self)
        self.capacity = capacity
        self.buffer = []

    def shouldFlush(self, record):
        return len(self.buffer) >= self.capacity

    def emit(self, record):
        self.buffer.append(record)
        if self.shouldFlush(record):
            self.flush()

    def flush(self):
        with self.lock:
            self.buffer = []

    def close(self):
        self.flush()
        Handler.close(self)


class MemoryHandler(BufferingHandler):
    def __init__(self, capacity, flushLevel=ERROR, target=None, flushOnClose=True):
        BufferingHandler.__init__(self, capacity)
        self.flushLevel = flushLevel
        self.target = target
        self.flushOnClose = flushOnClose

    def shouldFlush(self, record):
        return len(self.buffer) >= self.capacity or record.levelno >= self.flushLevel

    def flush(self):
        with self.lock:
            if self.target is not None:
                for record in self.buffer:
                    self.target.handle(record)
                self.buffer = []

    def close(self):
        if self.flushOnClose:
            self.flush()
        with self.lock:
            self.target = None
            BufferingHandler.close(self)


_handlers = {}


# ---------------- Logger / Manager ---------------- #

class PlaceHolder:
    def __init__(self, alogger):
        self.loggerMap = {alogger: None}

    def append(self, alogger):
        self.loggerMap[alogger] = None


class Manager:
    def __init__(self, rootnode):
        self.root = rootnode
        self.disable = 0
        self.emittedNoHandlerWarning = False
        self.loggerDict = {}
        self.loggerClass = None

    def getLogger(self, name):
        if not isinstance(name, str):
            raise TypeError("A logger name must be a string")
        if name in self.loggerDict:
            existing = self.loggerDict[name]
            if isinstance(existing, PlaceHolder):
                cls = self.loggerClass or Logger
                logger = cls(name)
                logger.manager = self
                self.loggerDict[name] = logger
                self._fixupChildren(existing, logger)
                self._fixupParents(logger)
                return logger
            return existing
        cls = self.loggerClass or Logger
        logger = cls(name)
        logger.manager = self
        self.loggerDict[name] = logger
        self._fixupParents(logger)
        return logger

    def setLoggerClass(self, klass):
        if not issubclass(klass, Logger):
            raise TypeError("logger class must be subclass of Logger")
        self.loggerClass = klass

    def _fixupParents(self, alogger):
        name = alogger.name
        i = name.rfind(".")
        rv = None
        while i > 0 and rv is None:
            substr = name[:i]
            if substr in self.loggerDict:
                obj = self.loggerDict[substr]
                if isinstance(obj, Logger):
                    rv = obj
                else:
                    obj.append(alogger)
            else:
                self.loggerDict[substr] = PlaceHolder(alogger)
            i = name.rfind(".", 0, i - 1)
        if rv is None:
            rv = self.root
        alogger.parent = rv

    def _fixupChildren(self, ph, alogger):
        name = alogger.name
        namelen = len(name)
        for c in ph.loggerMap.keys():
            if c.parent.name[:namelen] != name:
                alogger.parent = c.parent
                c.parent = alogger


class Logger(Filterer):
    manager = None

    def __init__(self, name, level=NOTSET):
        Filterer.__init__(self)
        self.name = name
        self.level = _check_level(level)
        self.parent = None
        self.propagate = True
        self.handlers = []
        self.disabled = False

    def setLevel(self, level):
        self.level = _check_level(level)

    def isEnabledFor(self, level):
        if self.disabled:
            return False
        if self.manager and self.manager.disable >= level:
            return False
        return level >= self.getEffectiveLevel()

    def getEffectiveLevel(self):
        logger = self
        while logger is not None:
            if logger.level:
                return logger.level
            logger = logger.parent
        return NOTSET

    def addHandler(self, hdlr):
        if hdlr not in self.handlers:
            self.handlers.append(hdlr)

    def removeHandler(self, hdlr):
        if hdlr in self.handlers:
            self.handlers.remove(hdlr)

    def hasHandlers(self):
        c = self
        while c is not None:
            if c.handlers:
                return True
            if not c.propagate:
                break
            c = c.parent
        return False

    def callHandlers(self, record):
        c = self
        found = 0
        while c is not None:
            for hdlr in c.handlers:
                found += 1
                if record.levelno >= hdlr.level:
                    hdlr.handle(record)
            if not c.propagate:
                break
            c = c.parent
        if found == 0 and root.handlers == []:
            # Last-ditch path matching CPython behaviour.
            sys.stderr.write(f"No handlers could be found for logger '{self.name}'\n")

    def handle(self, record):
        if self.disabled:
            return
        if self.filter(record):
            self.callHandlers(record)

    def makeRecord(self, name, level, fn, lno, msg, args, exc_info, func=None,
                   extra=None, sinfo=None):
        rv = LogRecord(name, level, fn, lno, msg, args, exc_info, func)
        rv.stack_info = sinfo
        if extra:
            for k, v in extra.items():
                rv.__dict__[k] = v
        return rv

    def _log(self, level, msg, args, exc_info=None, extra=None, stack_info=False,
             stacklevel=1):
        try:
            fn = sys._getframe(stacklevel + 1).f_code.co_filename
            lno = sys._getframe(stacklevel + 1).f_lineno
            func = sys._getframe(stacklevel + 1).f_code.co_name
        except Exception:
            fn, lno, func = "(unknown file)", 0, "(unknown function)"
        if exc_info is True:
            exc_info = sys.exc_info()
        rec = self.makeRecord(self.name, level, fn, lno, msg, args, exc_info, func, extra)
        self.handle(rec)

    def debug(self, msg, *args, **kwargs):
        if self.isEnabledFor(DEBUG):
            self._log(DEBUG, msg, args, **kwargs)

    def info(self, msg, *args, **kwargs):
        if self.isEnabledFor(INFO):
            self._log(INFO, msg, args, **kwargs)

    def warning(self, msg, *args, **kwargs):
        if self.isEnabledFor(WARNING):
            self._log(WARNING, msg, args, **kwargs)

    warn = warning

    def error(self, msg, *args, **kwargs):
        if self.isEnabledFor(ERROR):
            self._log(ERROR, msg, args, **kwargs)

    def critical(self, msg, *args, **kwargs):
        if self.isEnabledFor(CRITICAL):
            self._log(CRITICAL, msg, args, **kwargs)

    fatal = critical

    def exception(self, msg, *args, exc_info=True, **kwargs):
        if self.isEnabledFor(ERROR):
            self._log(ERROR, msg, args, exc_info=exc_info, **kwargs)

    def log(self, level, msg, *args, **kwargs):
        if self.isEnabledFor(level):
            self._log(level, msg, args, **kwargs)

    def getChild(self, suffix):
        if not isinstance(suffix, str):
            raise TypeError("suffix must be a str")
        if self is not root:
            suffix = ".".join((self.name, suffix))
        return self.manager.getLogger(suffix) if self.manager else getLogger(suffix)


class RootLogger(Logger):
    def __init__(self, level):
        Logger.__init__(self, "root", level)

    def __reduce__(self):
        return (getLogger, ())


root = RootLogger(WARNING)
Logger.root = root
Logger.manager = Manager(Logger.root)
root.manager = Logger.manager


def getLogger(name=None):
    if name is None or name == root.name:
        return root
    return Logger.manager.getLogger(name)


def disable(level=CRITICAL):
    root.manager.disable = _check_level(level) if level is not None else 0


def shutdown():
    for h in list(_handlers.values()):
        try:
            h.close()
        except Exception:
            pass


def captureWarnings(capture):
    # Simplified: real implementation re-routes the warnings module.
    pass


# ---------------- module-level convenience ---------------- #

def basicConfig(**kwargs):
    with _lock:
        force = kwargs.pop("force", False)
        encoding = kwargs.pop("encoding", None)
        errors = kwargs.pop("errors", None)
        if force:
            for h in list(root.handlers):
                root.removeHandler(h)
                h.close()
        if root.handlers:
            return
        handlers = kwargs.pop("handlers", None)
        if handlers is None:
            stream = kwargs.pop("stream", None)
            filename = kwargs.pop("filename", None)
            filemode = kwargs.pop("filemode", "a")
            if filename:
                h = FileHandler(filename, filemode, encoding=encoding, errors=errors)
            else:
                h = StreamHandler(stream)
            handlers = [h]
        fmt = kwargs.pop("format", BASIC_FORMAT)
        dfs = kwargs.pop("datefmt", None)
        style = kwargs.pop("style", "%")
        formatter = Formatter(fmt, dfs, style)
        for h in handlers:
            if h.formatter is None:
                h.setFormatter(formatter)
            root.addHandler(h)
        level = kwargs.pop("level", None)
        if level is not None:
            root.setLevel(level)


def log(level, msg, *args, **kwargs):
    if len(root.handlers) == 0:
        basicConfig()
    root.log(level, msg, *args, **kwargs)


def debug(msg, *args, **kwargs):
    if len(root.handlers) == 0:
        basicConfig()
    root.debug(msg, *args, **kwargs)


def info(msg, *args, **kwargs):
    if len(root.handlers) == 0:
        basicConfig()
    root.info(msg, *args, **kwargs)


def warning(msg, *args, **kwargs):
    if len(root.handlers) == 0:
        basicConfig()
    root.warning(msg, *args, **kwargs)


warn = warning


def error(msg, *args, **kwargs):
    if len(root.handlers) == 0:
        basicConfig()
    root.error(msg, *args, **kwargs)


def critical(msg, *args, **kwargs):
    if len(root.handlers) == 0:
        basicConfig()
    root.critical(msg, *args, **kwargs)


fatal = critical


def exception(msg, *args, exc_info=True, **kwargs):
    if len(root.handlers) == 0:
        basicConfig()
    root.exception(msg, *args, exc_info=exc_info, **kwargs)


raiseExceptions = True


import sys as _sys
logging = _sys.modules[__name__]
