"""WeavePy `inspect` — introspection helpers.

Implements the CPython-shaped API for the parts that user code reaches
for most: predicates (`isfunction`, `ismethod`, `isclass`, ...),
signature introspection (`signature`, `Signature`, `Parameter`),
source utilities (`getsource`, `getsourcefile`, `getsourcelines`),
frame walking (`currentframe`, `stack`, `trace`), and class
introspection (`getmro`, `getmembers`).
"""

import sys
import linecache


__all__ = [
    "isfunction",
    "ismethod",
    "ismodule",
    "isclass",
    "isbuiltin",
    "isroutine",
    "isgenerator",
    "isgeneratorfunction",
    "iscoroutine",
    "iscoroutinefunction",
    "isasyncgen",
    "isasyncgenfunction",
    "istraceback",
    "isframe",
    "iscode",
    "isabstract",
    "ismemberdescriptor",
    "isgetsetdescriptor",
    "isdatadescriptor",
    "ismethoddescriptor",
    "currentframe",
    "stack",
    "trace",
    "getframeinfo",
    "getsource",
    "getsourcefile",
    "getsourcelines",
    "getfile",
    "getmodule",
    "getmro",
    "getmembers",
    "getargspec",
    "getfullargspec",
    "signature",
    "Signature",
    "Parameter",
    "BoundArguments",
    "FrameInfo",
    "Traceback",
    "CO_OPTIMIZED",
    "CO_NEWLOCALS",
    "CO_VARARGS",
    "CO_VARKEYWORDS",
    "CO_NESTED",
    "CO_GENERATOR",
    "CO_NOFREE",
    "CO_COROUTINE",
    "CO_ITERABLE_COROUTINE",
    "CO_ASYNC_GENERATOR",
]


# Code-object flags. Keep in sync with weavepy-compiler/src/code.rs.
CO_OPTIMIZED = 0x0001
CO_NEWLOCALS = 0x0002
CO_VARARGS = 0x0004
CO_VARKEYWORDS = 0x0008
CO_NESTED = 0x0010
CO_GENERATOR = 0x0020
CO_NOFREE = 0x0040
CO_COROUTINE = 0x0100
CO_ITERABLE_COROUTINE = 0x0200
CO_ASYNC_GENERATOR = 0x0400


def _safe_type_name(t):
    return getattr(t, "__name__", repr(t))


# ---------------- predicates ---------------- #

def _has_attrs(obj, *names):
    for n in names:
        if not hasattr(obj, n):
            return False
    return True


def cleandoc(doc):
    """Clean up indentation from docstrings (CPython ``inspect.cleandoc``).

    Any leading whitespace is removed from the first line; the minimum
    indentation of subsequent non-blank lines is removed; leading and
    trailing blank lines are dropped.
    """
    if not doc:
        return doc
    lines = doc.expandtabs().split('\n')
    margin = None
    for line in lines[1:]:
        content = len(line.lstrip())
        if content:
            indent = len(line) - content
            margin = indent if margin is None else min(margin, indent)
    if lines:
        lines[0] = lines[0].lstrip()
    if margin is not None:
        for i in range(1, len(lines)):
            lines[i] = lines[i][margin:]
    while lines and not lines[-1]:
        lines.pop()
    while lines and not lines[0]:
        lines.pop(0)
    return '\n'.join(lines)


def getdoc(obj):
    """Return the cleaned-up documentation string for *obj* (or None)."""
    try:
        doc = obj.__doc__
    except AttributeError:
        return None
    if doc is None:
        try:
            cls = type(obj)
            for base in getattr(cls, "__mro__", (cls,)):
                doc = getattr(base, "__doc__", None)
                if doc is not None:
                    break
        except Exception:
            return None
    if not isinstance(doc, str):
        return None
    return cleandoc(doc)


def isfunction(obj):
    return type(obj).__name__ == "function"


def ismethod(obj):
    return _has_attrs(obj, "__func__", "__self__")


def ismodule(obj):
    return type(obj).__name__ == "module"


def isclass(obj):
    return isinstance(obj, type)


def isbuiltin(obj):
    return type(obj).__name__ in ("builtin_function_or_method", "builtin_function")


def isroutine(obj):
    return isfunction(obj) or ismethod(obj) or isbuiltin(obj)


def isgenerator(obj):
    return type(obj).__name__ == "generator"


def isgeneratorfunction(obj):
    code = getattr(obj, "__code__", None)
    if code is None:
        return False
    return bool(getattr(code, "co_flags", 0) & CO_GENERATOR)


def iscoroutine(obj):
    return type(obj).__name__ == "coroutine"


def iscoroutinefunction(obj):
    code = getattr(obj, "__code__", None)
    if code is None:
        return False
    return bool(getattr(code, "co_flags", 0) & CO_COROUTINE)


def isasyncgen(obj):
    return type(obj).__name__ == "async_generator"


def isasyncgenfunction(obj):
    code = getattr(obj, "__code__", None)
    if code is None:
        return False
    return bool(getattr(code, "co_flags", 0) & CO_ASYNC_GENERATOR)


def istraceback(obj):
    return type(obj).__name__ == "traceback"


def isframe(obj):
    return type(obj).__name__ == "frame"


def iscode(obj):
    return type(obj).__name__ == "code"


def isabstract(obj):
    return bool(getattr(obj, "__abstractmethods__", None))


def ismemberdescriptor(obj):
    return False


def isgetsetdescriptor(obj):
    return False


def isdatadescriptor(obj):
    return hasattr(obj, "__set__") and hasattr(obj, "__get__")


def ismethoddescriptor(obj):
    return hasattr(obj, "__get__") and not hasattr(obj, "__set__") and not hasattr(obj, "__delete__")


# ---------------- frames / stack ---------------- #

class Traceback:
    """The slice of a frame's metadata that's relevant for tracebacks."""

    def __init__(self, filename, lineno, function, code_context, index):
        self.filename = filename
        self.lineno = lineno
        self.function = function
        self.code_context = code_context
        self.index = index

    def __iter__(self):
        return iter((self.filename, self.lineno, self.function, self.code_context, self.index))


class FrameInfo(Traceback):
    def __init__(self, frame, filename, lineno, function, code_context, index):
        super().__init__(filename, lineno, function, code_context, index)
        self.frame = frame

    def __iter__(self):
        return iter((self.frame, self.filename, self.lineno, self.function,
                     self.code_context, self.index))


def currentframe():
    if not hasattr(sys, "_getframe"):
        return None
    try:
        return sys._getframe(1)
    except ValueError:
        return None


def getframeinfo(frame, context=1):
    if istraceback(frame):
        lineno = frame.tb_lineno
        frame = frame.tb_frame
    else:
        lineno = frame.f_lineno
    code = frame.f_code
    filename = getattr(code, "co_filename", "<unknown>")
    function = getattr(code, "co_name", "<unknown>")
    code_context = None
    index = None
    if context > 0:
        try:
            lines = linecache.getlines(filename) or []
            if lines:
                start = max(lineno - 1 - context // 2, 0)
                end = min(start + context, len(lines))
                code_context = lines[start:end]
                index = lineno - 1 - start
        except Exception:
            pass
    return Traceback(filename, lineno, function, code_context, index)


def stack(context=1):
    f = currentframe()
    if f is not None:
        f = f.f_back
    out = []
    while f is not None:
        tb = getframeinfo(f, context)
        out.append(FrameInfo(f, tb.filename, tb.lineno, tb.function, tb.code_context, tb.index))
        f = f.f_back
    return out


def trace(context=1):
    tb = sys.exc_info()[2]
    out = []
    while tb is not None:
        info = getframeinfo(tb, context)
        out.append(FrameInfo(tb.tb_frame, info.filename, info.lineno, info.function,
                             info.code_context, info.index))
        tb = tb.tb_next
    return out


# ---------------- source utilities ---------------- #

def getfile(obj):
    if ismodule(obj):
        f = getattr(obj, "__file__", None)
        if f is not None:
            return f
        raise TypeError(f"<module {obj.__name__!r}> is a built-in module")
    if isclass(obj):
        if hasattr(obj, "__module__"):
            mod = sys.modules.get(obj.__module__)
            if mod is not None and hasattr(mod, "__file__"):
                return mod.__file__
        raise TypeError(f"source code not available for {obj!r}")
    if isfunction(obj) or ismethod(obj):
        code = getattr(obj, "__code__", None) or getattr(getattr(obj, "__func__", None), "__code__", None)
        if code is not None:
            return code.co_filename
    if iscode(obj):
        return obj.co_filename
    if isframe(obj):
        return obj.f_code.co_filename
    if istraceback(obj):
        return obj.tb_frame.f_code.co_filename
    raise TypeError(f"source code not available for {obj!r}")


def getsourcefile(obj):
    try:
        filename = getfile(obj)
    except TypeError:
        return None
    if filename.endswith((".py", ".pyw")):
        return filename
    return None


def getsourcelines(obj):
    filename = getsourcefile(obj)
    if filename is None:
        raise OSError("source not available")
    source = linecache.getlines(filename)
    if not source:
        raise OSError(f"could not get source for {obj!r}")
    if isfunction(obj) or ismethod(obj):
        code = getattr(obj, "__code__", None) or obj.__func__.__code__
        return _block_around(source, code.co_firstlineno - 1)
    if iscode(obj):
        return _block_around(source, obj.co_firstlineno - 1)
    if isclass(obj):
        return _class_block(source, obj.__name__)
    return source, 1


def getsource(obj):
    lines, _ = getsourcelines(obj)
    return "".join(lines)


def _block_around(lines, start):
    if start < 0 or start >= len(lines):
        return [], 1
    head = lines[start]
    indent = len(head) - len(head.lstrip(" \t"))
    out = [head]
    i = start + 1
    while i < len(lines):
        line = lines[i]
        if line.strip() == "":
            out.append(line)
            i += 1
            continue
        cur_indent = len(line) - len(line.lstrip(" \t"))
        if cur_indent <= indent and line.strip() and not line.lstrip().startswith("#"):
            break
        out.append(line)
        i += 1
    return out, start + 1


def _class_block(lines, name):
    pattern = "class " + name
    for i, line in enumerate(lines):
        stripped = line.lstrip()
        if stripped.startswith(pattern):
            return _block_around(lines, i)
    return [], 1


# ---------------- module & members ---------------- #

def getmodule(obj, _filename=None):
    if ismodule(obj):
        return obj
    if hasattr(obj, "__module__"):
        return sys.modules.get(obj.__module__)
    if _filename is not None:
        for m in list(sys.modules.values()):
            if getattr(m, "__file__", None) == _filename:
                return m
    return None


def getmro(cls):
    return tuple(cls.__mro__)


def getmembers(obj, predicate=None):
    out = []
    seen = set()
    mro = ()
    if isclass(obj):
        try:
            mro = (obj,) + tuple(obj.__mro__[1:])
        except Exception:
            mro = (obj,)
    for klass in mro:
        try:
            for k, v in vars(klass).items():
                if k in seen:
                    continue
                seen.add(k)
                if predicate is None or predicate(v):
                    out.append((k, v))
        except Exception:
            pass
    for name in dir(obj):
        if name in seen:
            continue
        try:
            value = getattr(obj, name)
        except AttributeError:
            continue
        seen.add(name)
        if predicate is None or predicate(value):
            out.append((name, value))
    out.sort(key=lambda kv: kv[0])
    return out


# ---------------- argspec / signature ---------------- #

class FullArgSpec:
    """Result of `getfullargspec`."""

    __slots__ = ("args", "varargs", "varkw", "defaults", "kwonlyargs",
                 "kwonlydefaults", "annotations")

    def __init__(self, args, varargs, varkw, defaults, kwonlyargs,
                 kwonlydefaults, annotations):
        self.args = args
        self.varargs = varargs
        self.varkw = varkw
        self.defaults = defaults
        self.kwonlyargs = kwonlyargs
        self.kwonlydefaults = kwonlydefaults
        self.annotations = annotations

    def __iter__(self):
        return iter((self.args, self.varargs, self.varkw, self.defaults,
                     self.kwonlyargs, self.kwonlydefaults, self.annotations))


def _func_of(obj):
    if ismethod(obj):
        return obj.__func__
    if isfunction(obj):
        return obj
    return None


def getfullargspec(func):
    f = _func_of(func)
    if f is None:
        raise TypeError(f"unsupported callable: {func!r}")
    code = f.__code__
    defaults = getattr(f, "__defaults__", None)
    kwdefaults = getattr(f, "__kwdefaults__", None) or {}
    annotations = getattr(f, "__annotations__", None) or {}
    flags = code.co_flags
    nargs = getattr(code, "co_argcount", 0)
    nkwonly = getattr(code, "co_kwonlyargcount", 0)
    varnames = list(getattr(code, "co_varnames", ()))
    # Fast-local layout (CPython): positional args, then keyword-only
    # args, then ``*args``, then ``**kwargs``.
    args = varnames[:nargs]
    idx = nargs
    kwonly = varnames[idx:idx + nkwonly]
    idx += nkwonly
    varargs = None
    varkw = None
    if flags & CO_VARARGS:
        if idx < len(varnames):
            varargs = varnames[idx]
            idx += 1
    if flags & CO_VARKEYWORDS:
        if idx < len(varnames):
            varkw = varnames[idx]
    return FullArgSpec(args, varargs, varkw, defaults, kwonly, kwdefaults, annotations)


def getargspec(func):
    spec = getfullargspec(func)
    return (spec.args, spec.varargs, spec.varkw, spec.defaults)


class _empty:
    """Marker for missing values in Parameter / Signature."""
    pass


class Parameter:
    POSITIONAL_ONLY = 0
    POSITIONAL_OR_KEYWORD = 1
    VAR_POSITIONAL = 2
    KEYWORD_ONLY = 3
    VAR_KEYWORD = 4

    empty = _empty

    __slots__ = ("_name", "_kind", "_default", "_annotation")

    def __init__(self, name, kind, *, default=_empty, annotation=_empty):
        self._name = name
        self._kind = kind
        self._default = default
        self._annotation = annotation

    @property
    def name(self):
        return self._name

    @property
    def kind(self):
        return self._kind

    @property
    def default(self):
        return self._default

    @property
    def annotation(self):
        return self._annotation

    def replace(self, *, name=None, kind=None, default=_empty, annotation=_empty):
        return Parameter(
            name if name is not None else self._name,
            kind if kind is not None else self._kind,
            default=default if default is not _empty else self._default,
            annotation=annotation if annotation is not _empty else self._annotation,
        )

    def __repr__(self):
        formatted = self._name
        if self._annotation is not _empty:
            formatted += f": {self._annotation!r}"
        if self._default is not _empty:
            formatted += f"={self._default!r}"
        if self._kind == Parameter.VAR_POSITIONAL:
            formatted = "*" + formatted
        elif self._kind == Parameter.VAR_KEYWORD:
            formatted = "**" + formatted
        return f"<Parameter {formatted!r}>"

    def __str__(self):
        out = self._name
        if self._kind == Parameter.VAR_POSITIONAL:
            out = "*" + out
        elif self._kind == Parameter.VAR_KEYWORD:
            out = "**" + out
        if self._annotation is not _empty:
            out += f": {self._annotation}"
        if self._default is not _empty:
            sep = " = " if self._annotation is not _empty else "="
            out += sep + repr(self._default)
        return out


class BoundArguments:
    def __init__(self, signature, arguments):
        self.signature = signature
        self.arguments = arguments

    @property
    def args(self):
        args = []
        for name, p in self.signature.parameters.items():
            if p.kind == Parameter.VAR_POSITIONAL:
                args.extend(self.arguments.get(name, ()))
                continue
            if p.kind in (Parameter.POSITIONAL_ONLY, Parameter.POSITIONAL_OR_KEYWORD):
                if name in self.arguments:
                    args.append(self.arguments[name])
                else:
                    break
            else:
                break
        return tuple(args)

    @property
    def kwargs(self):
        kwargs = {}
        passed_to_args = False
        for name, p in self.signature.parameters.items():
            if p.kind == Parameter.VAR_POSITIONAL:
                passed_to_args = True
                continue
            if not passed_to_args and p.kind in (Parameter.POSITIONAL_ONLY, Parameter.POSITIONAL_OR_KEYWORD):
                continue
            if p.kind == Parameter.VAR_KEYWORD:
                kwargs.update(self.arguments.get(name, {}))
                continue
            if name in self.arguments and p.kind != Parameter.POSITIONAL_ONLY:
                kwargs[name] = self.arguments[name]
        return kwargs

    def apply_defaults(self):
        for name, p in self.signature.parameters.items():
            if name in self.arguments:
                continue
            if p.default is not _empty:
                self.arguments[name] = p.default
            elif p.kind == Parameter.VAR_POSITIONAL:
                self.arguments[name] = ()
            elif p.kind == Parameter.VAR_KEYWORD:
                self.arguments[name] = {}


class Signature:
    empty = _empty

    __slots__ = ("_parameters", "_return_annotation")

    def __init__(self, parameters=None, *, return_annotation=_empty):
        params = {}
        if parameters is not None:
            for p in parameters:
                params[p.name] = p
        self._parameters = params
        self._return_annotation = return_annotation

    @property
    def parameters(self):
        return self._parameters

    @property
    def return_annotation(self):
        return self._return_annotation

    def replace(self, *, parameters=_empty, return_annotation=_empty):
        params = list(self._parameters.values()) if parameters is _empty else list(parameters)
        ret = self._return_annotation if return_annotation is _empty else return_annotation
        return Signature(params, return_annotation=ret)

    def bind(self, *args, **kwargs):
        return self._bind(args, kwargs, partial=False)

    def bind_partial(self, *args, **kwargs):
        return self._bind(args, kwargs, partial=True)

    def _bind(self, args, kwargs, partial):
        arguments = {}
        params = list(self._parameters.values())
        # Map positional args.
        pos = 0
        for p in params:
            if pos >= len(args):
                break
            if p.kind == Parameter.VAR_POSITIONAL:
                arguments[p.name] = tuple(args[pos:])
                pos = len(args)
                break
            if p.kind in (Parameter.POSITIONAL_ONLY, Parameter.POSITIONAL_OR_KEYWORD):
                arguments[p.name] = args[pos]
                pos += 1
            else:
                break
        if pos < len(args):
            if not partial:
                raise TypeError("too many positional arguments")
        # Map keyword args.
        for name, value in kwargs.items():
            p = self._parameters.get(name)
            if p is None:
                var_kw = next((q for q in params if q.kind == Parameter.VAR_KEYWORD), None)
                if var_kw is None:
                    if not partial:
                        raise TypeError(f"got an unexpected keyword argument {name!r}")
                    continue
                arguments.setdefault(var_kw.name, {})
                arguments[var_kw.name][name] = value
                continue
            if p.kind in (Parameter.VAR_POSITIONAL, Parameter.POSITIONAL_ONLY):
                if not partial:
                    raise TypeError(f"{name!r} cannot be passed by keyword")
                continue
            if p.name in arguments:
                if not partial:
                    raise TypeError(f"multiple values for argument {name!r}")
                continue
            arguments[p.name] = value
        # Required parameters check (skip if partial).
        if not partial:
            for p in params:
                if (
                    p.name not in arguments
                    and p.kind not in (Parameter.VAR_POSITIONAL, Parameter.VAR_KEYWORD)
                    and p.default is _empty
                ):
                    raise TypeError(f"missing a required argument: {p.name!r}")
        return BoundArguments(self, arguments)

    def __str__(self):
        parts = []
        kind_seen = None
        for p in self._parameters.values():
            if p.kind == Parameter.KEYWORD_ONLY and kind_seen != Parameter.VAR_POSITIONAL and kind_seen != Parameter.KEYWORD_ONLY:
                parts.append("*")
            parts.append(str(p))
            kind_seen = p.kind
        ret = ""
        if self._return_annotation is not _empty:
            ret = f" -> {self._return_annotation!r}"
        return "(" + ", ".join(parts) + ")" + ret

    @classmethod
    def from_callable(cls, func):
        return signature(func)


def signature(callable_):
    if isclass(callable_):
        init = getattr(callable_, "__init__", None)
        if init is not None and init is not object.__init__:
            sig = signature(init)
            params = [p for name, p in sig.parameters.items() if name != "self"]
            return Signature(params, return_annotation=callable_)
        return Signature([])
    if ismethod(callable_):
        sig = signature(callable_.__func__)
        params = [p for name, p in sig.parameters.items() if name != "self"]
        return Signature(params, return_annotation=sig.return_annotation)
    if not isfunction(callable_):
        # Best effort: return an "unknown" signature.
        return Signature([Parameter("args", Parameter.VAR_POSITIONAL),
                          Parameter("kwargs", Parameter.VAR_KEYWORD)])
    spec = getfullargspec(callable_)
    params = []
    defaults = spec.defaults or ()
    n_defaults = len(defaults)
    n_args = len(spec.args)
    for i, name in enumerate(spec.args):
        if i >= n_args - n_defaults:
            default = defaults[i - (n_args - n_defaults)]
        else:
            default = _empty
        annotation = spec.annotations.get(name, _empty)
        params.append(Parameter(name, Parameter.POSITIONAL_OR_KEYWORD,
                                default=default, annotation=annotation))
    if spec.varargs:
        params.append(Parameter(spec.varargs, Parameter.VAR_POSITIONAL,
                                annotation=spec.annotations.get(spec.varargs, _empty)))
    for name in spec.kwonlyargs:
        params.append(Parameter(name, Parameter.KEYWORD_ONLY,
                                default=spec.kwonlydefaults.get(name, _empty),
                                annotation=spec.annotations.get(name, _empty)))
    if spec.varkw:
        params.append(Parameter(spec.varkw, Parameter.VAR_KEYWORD,
                                annotation=spec.annotations.get(spec.varkw, _empty)))
    return Signature(params, return_annotation=spec.annotations.get("return", _empty))
