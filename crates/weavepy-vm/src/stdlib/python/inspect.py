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
import types
import functools


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
    "ismethodwrapper",
    "classify_class_attrs",
    "Attribute",
    "getclasstree",
    "walktree",
    "getcomments",
    "getabsfile",
    "getattr_static",
    "indentsize",
    "findsource",
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
    "formatannotation",
    "unwrap",
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
    "get_annotations",
]


# Code-object flags — CPython's values (keep in sync with
# `code_flags` in weavepy-vm/src/builtins.rs).
CO_OPTIMIZED = 0x0001
CO_NEWLOCALS = 0x0002
CO_VARARGS = 0x0004
CO_VARKEYWORDS = 0x0008
CO_NESTED = 0x0010
CO_GENERATOR = 0x0020
CO_NOFREE = 0x0040
CO_COROUTINE = 0x0080
CO_ITERABLE_COROUTINE = 0x0100
CO_ASYNC_GENERATOR = 0x0200


def _safe_type_name(t):
    return getattr(t, "__name__", repr(t))


def get_annotations(obj, *, globals=None, locals=None, eval_str=False):
    """Compute the annotations dict for an object.

    Verbatim port of CPython 3.13's ``inspect.get_annotations``: ``obj``
    may be a callable, class, or module, and the result is always a
    freshly-created dict. ``dataclasses`` relies on this to read a
    class's own ``__annotations__`` while ignoring inherited ones.
    """
    if isinstance(obj, type):
        # class
        obj_dict = getattr(obj, '__dict__', None)
        if obj_dict and hasattr(obj_dict, 'get'):
            ann = obj_dict.get('__annotations__', None)
            if isinstance(ann, types.GetSetDescriptorType):
                ann = None
        else:
            ann = None

        obj_globals = None
        module_name = getattr(obj, '__module__', None)
        if module_name:
            module = sys.modules.get(module_name, None)
            if module:
                obj_globals = getattr(module, '__dict__', None)
        obj_locals = dict(vars(obj))
        unwrap = obj
    elif isinstance(obj, types.ModuleType):
        # module
        ann = getattr(obj, '__annotations__', None)
        obj_globals = getattr(obj, '__dict__')
        obj_locals = None
        unwrap = None
    elif callable(obj):
        # this includes types.Function, types.BuiltinFunctionType,
        # types.BuiltinMethodType, functools.partial, functools.singledispatch,
        # "class funclike" from Lib/test/test_inspect... on and on it goes.
        ann = getattr(obj, '__annotations__', None)
        obj_globals = getattr(obj, '__globals__', None)
        obj_locals = None
        unwrap = obj
    else:
        raise TypeError(f"{obj!r} is not a module, class, or callable.")

    if ann is None:
        return {}

    if not isinstance(ann, dict):
        raise ValueError(f"{obj!r}.__annotations__ is neither a dict nor None")

    if not ann:
        return {}

    if not eval_str:
        return dict(ann)

    if unwrap is not None:
        while True:
            if hasattr(unwrap, '__wrapped__'):
                unwrap = unwrap.__wrapped__
                continue
            if isinstance(unwrap, functools.partial):
                unwrap = unwrap.func
                continue
            break
        if hasattr(unwrap, "__globals__"):
            obj_globals = unwrap.__globals__

    if globals is None:
        globals = obj_globals
    if locals is None:
        locals = obj_locals or {}

    # "Inject" type parameters into the local namespace
    # (unless they are shadowed by assignments *in* the local namespace),
    # as a way of emulating annotation scopes when calling `eval()`
    if type_params := getattr(obj, "__type_params__", ()):
        locals = {param.__name__: param for param in type_params} | locals

    # PEP 646 star-unpack rewriting lives in `typing` on CPython 3.13; fall
    # back to a no-op when that internal helper isn't available.
    try:
        from typing import _rewrite_star_unpack as _rewrite
    except ImportError:
        def _rewrite(value):
            return value

    return_value = {
        key: value if not isinstance(value, str)
        else eval(_rewrite(value), globals, locals)
        for key, value in ann.items() }
    return return_value


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


def isawaitable(obj):
    """True for coroutines, iterable-coroutine generators, and objects
    with a `__await__` method (CPython `inspect.isawaitable`)."""
    if iscoroutine(obj):
        return True
    tn = type(obj).__name__
    if tn == "generator":
        code = getattr(obj, "gi_code", None)
        return bool(getattr(code, "co_flags", 0) & CO_ITERABLE_COROUTINE)
    # CPython also accepts any object implementing __await__ (including
    # its own async_generator_asend/athrow awaitables, which expose it).
    return hasattr(type(obj), "__await__") or hasattr(obj, "__await__")


def isasyncgenfunction(obj):
    code = getattr(obj, "__code__", None)
    if code is None:
        return False
    return bool(getattr(code, "co_flags", 0) & CO_ASYNC_GENERATOR)


def istraceback(obj):
    return type(obj).__name__ == "traceback"


GEN_CREATED = 'GEN_CREATED'
GEN_RUNNING = 'GEN_RUNNING'
GEN_SUSPENDED = 'GEN_SUSPENDED'
GEN_CLOSED = 'GEN_CLOSED'


def getgeneratorstate(generator):
    """Get current state of a generator-iterator."""
    if generator.gi_running:
        return GEN_RUNNING
    if generator.gi_suspended:
        return GEN_SUSPENDED
    if generator.gi_frame is None:
        return GEN_CLOSED
    return GEN_CREATED


CORO_CREATED = 'CORO_CREATED'
CORO_RUNNING = 'CORO_RUNNING'
CORO_SUSPENDED = 'CORO_SUSPENDED'
CORO_CLOSED = 'CORO_CLOSED'


def getcoroutinestate(coroutine):
    """Get current state of a coroutine."""
    if coroutine.cr_running:
        return CORO_RUNNING
    if coroutine.cr_suspended:
        return CORO_SUSPENDED
    if coroutine.cr_frame is None:
        return CORO_CLOSED
    return CORO_CREATED


AGEN_CREATED = 'AGEN_CREATED'
AGEN_RUNNING = 'AGEN_RUNNING'
AGEN_SUSPENDED = 'AGEN_SUSPENDED'
AGEN_CLOSED = 'AGEN_CLOSED'


def getasyncgenstate(agen):
    """Get current state of an asynchronous generator."""
    if agen.ag_running:
        return AGEN_RUNNING
    if agen.ag_suspended:
        return AGEN_SUSPENDED
    if agen.ag_frame is None:
        return AGEN_CLOSED
    return AGEN_CREATED


def getgeneratorlocals(generator):
    """Get the mapping of generator local variables to their current values."""
    if not isgenerator(generator):
        raise TypeError("{!r} is not a Python generator".format(generator))
    frame = getattr(generator, "gi_frame", None)
    if frame is not None:
        return generator.gi_frame.f_locals
    return {}


def getcoroutinelocals(coroutine):
    """Get the mapping of coroutine local variables to their current values."""
    frame = getattr(coroutine, "cr_frame", None)
    if frame is not None:
        return frame.f_locals
    return {}


def getasyncgenlocals(agen):
    """Get the mapping of asynchronous generator local variables to their
    current values."""
    if not isasyncgen(agen):
        raise TypeError(f"{agen!r} is not a Python async generator")
    frame = getattr(agen, "ag_frame", None)
    if frame is not None:
        return agen.ag_frame.f_locals
    return {}


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
    """CPython semantics: data descriptors define `__set__` or
    `__delete__` *on their type* (properties, slots, C getsets)."""
    if isclass(obj) or ismethod(obj) or isfunction(obj):
        # mutual exclusion, as in CPython
        return False
    tp = type(obj)
    return hasattr(tp, "__set__") or hasattr(tp, "__delete__")


def ismethoddescriptor(obj):
    """CPython semantics: non-data descriptors with a `__get__` whose
    type carries neither `__set__` nor `__delete__`."""
    if isclass(obj) or ismethod(obj) or isfunction(obj):
        # mutual exclusion, as in CPython
        return False
    tp = type(obj)
    return (hasattr(tp, "__get__")
            and not hasattr(tp, "__set__")
            and not hasattr(tp, "__delete__"))


def ismethodwrapper(obj):
    """Return true if the object is a method wrapper (bound slot wrapper)."""
    return isinstance(obj, types.MethodWrapperType)


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


def getabsfile(obj, _filename=None):
    """Return an absolute path to the source or compiled file for an object.

    The idea is for each object to have a unique origin, so this routine
    normalizes the result as much as possible. (CPython `inspect.getabsfile`.)
    """
    import os
    if _filename is None:
        _filename = getsourcefile(obj) or getfile(obj)
    return os.path.normcase(os.path.abspath(_filename))


def indentsize(line):
    """Return the indent size, in spaces, at the start of a line of text."""
    expline = line.expandtabs()
    return len(expline) - len(expline.lstrip())


def findsource(obj):
    """Return the entire source file and starting line number for an object.

    The argument may be a module, class, method, function, traceback, frame,
    or code object.  The source code is returned as a list of all the lines
    in the file and the line number indexes a line in that list.  An OSError
    is raised if the source code cannot be retrieved.
    """
    filename = getsourcefile(obj)
    if filename is None:
        raise OSError("source code not available")
    lines = linecache.getlines(filename)
    if not lines:
        raise OSError("could not get source code")
    if ismodule(obj):
        return lines, 0
    if isclass(obj):
        block, lnum = _class_block(lines, obj.__name__)
        if not block:
            raise OSError("could not find class definition")
        return lines, lnum - 1
    if ismethod(obj):
        obj = obj.__func__
    if isfunction(obj):
        obj = getattr(obj, "__code__", None)
    if istraceback(obj):
        obj = obj.tb_frame
    if isframe(obj):
        obj = obj.f_code
    if iscode(obj):
        lnum = obj.co_firstlineno - 1
        if lnum < 0 or lnum >= len(lines):
            raise OSError("lineno is out of bounds")
        return lines, lnum
    raise OSError("could not find code object")


def getcomments(obj):
    """Get lines of comments immediately preceding an object's source code.

    Returns None when source can't be found. (CPython `inspect.getcomments`.)
    """
    try:
        lines, lnum = findsource(obj)
    except (OSError, TypeError):
        return None

    if ismodule(obj):
        # Look for a comment block at the top of the file.
        start = 0
        if lines and lines[0][:2] == '#!':
            start = 1
        while start < len(lines) and lines[start].strip() in ('', '#'):
            start = start + 1
        if start < len(lines) and lines[start][:1] == '#':
            comments = []
            end = start
            while end < len(lines) and lines[end][:1] == '#':
                comments.append(lines[end].expandtabs())
                end = end + 1
            return ''.join(comments)

    # Look for a comment block preceding the object.
    elif lnum > 0:
        indent = indentsize(lines[lnum])
        end = lnum - 1
        if end >= 0 and lines[end].lstrip()[:1] == '#' and \
                indentsize(lines[end]) == indent:
            comments = [lines[end].expandtabs().lstrip()]
            if end > 0:
                end = end - 1
                comment = lines[end].expandtabs().lstrip()
                while comment[:1] == '#' and indentsize(lines[end]) == indent:
                    comments[:0] = [comment]
                    end = end - 1
                    if end < 0:
                        break
                    comment = lines[end].expandtabs().lstrip()
            while comments and comments[0].strip() == '#':
                comments[:1] = []
            while comments and comments[-1].strip() == '#':
                comments[-1:] = []
            return ''.join(comments)
    return None


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
    # CPython `inspect._getmembers`: names come from `dir(obj)` (plus
    # DynamicClassAttributes from base dicts when `obj` is a class) —
    # *not* from walking every MRO `__dict__`, so a metaclass `__dir__`
    # (e.g. `EnumType.__dir__`) controls what is reported.
    results = []
    processed = set()
    names = dir(obj)
    if isclass(obj):
        mro = getmro(obj)
        try:
            for base in obj.__bases__:
                for k, v in base.__dict__.items():
                    if isinstance(v, types.DynamicClassAttribute):
                        names.append(k)
        except AttributeError:
            pass
    else:
        mro = ()
    for key in names:
        try:
            value = getattr(obj, key)
            if key in processed:
                raise AttributeError
        except AttributeError:
            for base in mro:
                if key in base.__dict__:
                    value = base.__dict__[key]
                    break
            else:
                # could be a (currently) missing slot member, or a buggy
                # __dir__; discard and move on
                continue
        if predicate is None or predicate(value):
            results.append((key, value))
        processed.add(key)
    results.sort(key=lambda pair: pair[0])
    return results


try:
    from collections import namedtuple as _namedtuple

    Attribute = _namedtuple('Attribute', 'name kind defining_class object')
except Exception:  # pragma: no cover - collections is always frozen
    Attribute = None


def classify_class_attrs(cls):
    """Return list of attribute-descriptor tuples.

    CPython `inspect.classify_class_attrs`: for each name in `dir(cls)`
    (plus DynamicClassAttributes found on the MRO), a 4-tuple of
    (name, kind, defining class, object). Kind is one of 'class method',
    'static method', 'property', 'method', 'data'.
    """
    mro = getmro(cls)
    metamro = getmro(type(cls))  # for attributes stored in the metaclass
    metamro = tuple(c for c in metamro if c not in (type, object))
    class_bases = (cls,) + tuple(mro)
    all_bases = class_bases + metamro
    names = dir(cls)
    # Add any DynamicClassAttributes to the list of names;
    # this may result in duplicate entries if, for example, a virtual
    # attribute with the same name as a DynamicClassAttribute exists.
    for base in mro:
        for k, v in base.__dict__.items():
            if isinstance(v, types.DynamicClassAttribute) and v.fget is not None:
                names.append(k)
    result = []
    processed = set()

    for name in names:
        # Get the object associated with the name, and where it was defined.
        homecls = None
        get_obj = None
        dict_obj = None
        if name not in processed:
            try:
                if name == '__dict__':
                    raise Exception("__dict__ is special, don't want the proxy")
                get_obj = getattr(cls, name)
            except Exception:
                pass
            else:
                homecls = getattr(get_obj, "__objclass__", homecls)
                if homecls not in class_bases:
                    # if the resulting object does not live somewhere in the
                    # mro, drop it and search the mro manually
                    homecls = None
                    last_cls = None
                    # first look in the classes
                    for srch_cls in class_bases:
                        srch_obj = getattr(srch_cls, name, None)
                        if srch_obj is get_obj:
                            last_cls = srch_cls
                    # then check the metaclasses
                    for srch_cls in metamro:
                        try:
                            srch_obj = srch_cls.__getattr__(cls, name)
                        except AttributeError:
                            continue
                        if srch_obj is get_obj:
                            last_cls = srch_cls
                    if last_cls is not None:
                        homecls = last_cls
        for base in all_bases:
            if name in base.__dict__:
                dict_obj = base.__dict__[name]
                if homecls not in metamro:
                    homecls = base
                break
        if name == '__class__' and dict_obj is None and homecls not in metamro:
            # CPython stores a `__class__` getset in `object.__dict__`;
            # WeavePy synthesizes the attribute instead of materializing
            # a dict entry, so the walk above can't find it — credit
            # `object`, as CPython does.
            homecls = object
        if homecls is None:
            # unable to locate the attribute anywhere, most likely due to
            # buggy custom __dir__; discard and move on
            continue
        obj = get_obj if get_obj is not None else dict_obj
        # Classify the object or its descriptor.
        if isinstance(dict_obj, (staticmethod, types.BuiltinMethodType)):
            kind = "static method"
            obj = dict_obj
        elif isinstance(dict_obj, (classmethod, types.ClassMethodDescriptorType)):
            kind = "class method"
            obj = dict_obj
        elif isinstance(dict_obj, property):
            kind = "property"
            obj = dict_obj
        elif isroutine(obj):
            kind = "method"
        else:
            kind = "data"
        result.append(Attribute(name, kind, homecls, obj))
        processed.add(name)
    return result


def walktree(classes, children, parent):
    """Recursive helper function for getclasstree()."""
    results = []
    classes.sort(key=lambda c: (c.__module__, c.__name__))
    for c in classes:
        results.append((c, c.__bases__))
        if c in children:
            results.append(walktree(children[c], children, c))
    return results


def getclasstree(classes, unique=False):
    """Arrange the given list of classes into a hierarchy of nested lists.

    Where a nested list appears, it contains classes derived from the class
    whose entry immediately precedes the list. (CPython `inspect.getclasstree`.)
    """
    children = {}
    roots = []
    for c in classes:
        if c.__bases__:
            for parent in c.__bases__:
                if parent not in children:
                    children[parent] = []
                if c not in children[parent]:
                    children[parent].append(c)
                if unique and parent in classes:
                    break
        elif c not in roots:
            roots.append(c)
    for parent in children:
        if parent not in classes:
            roots.append(parent)
    return walktree(roots, children, None)


_static_sentinel = object()


def _static_lookup_in_dict(obj_dict, attr):
    try:
        return obj_dict[attr], True
    except (KeyError, TypeError):
        return None, False


def getattr_static(obj, attr, default=_static_sentinel):
    """Retrieve attributes without triggering dynamic lookup via the
    descriptor protocol, __getattr__ or __getattribute__.

    Behavioural port of CPython `inspect.getattr_static`: walk the
    instance `__dict__` and the type's MRO dictionaries directly. Data
    descriptors found on the type take precedence over instance
    attributes, mirroring `object.__getattribute__`'s static order.
    """
    instance_result = _static_sentinel
    klass = type(obj)
    if not isclass(obj):
        dict_attr, found = _static_lookup_in_dict(
            getattr(obj, "__dict__", {}) or {}, attr)
        if found:
            instance_result = dict_attr
    else:
        klass = obj

    klass_result = _static_sentinel
    for entry in getmro(klass):
        d = entry.__dict__
        if attr in d:
            klass_result = d[attr]
            break

    if instance_result is not _static_sentinel and \
            klass_result is not _static_sentinel:
        # A data descriptor on the class shadows the instance dict.
        if hasattr(type(klass_result), "__set__") or \
                hasattr(type(klass_result), "__delete__"):
            return klass_result
        return instance_result
    if instance_result is not _static_sentinel:
        return instance_result
    if klass_result is not _static_sentinel:
        return klass_result

    if isclass(obj):
        # Search the metaclass MRO as well.
        for entry in getmro(type(obj)):
            d = entry.__dict__
            if attr in d:
                return d[attr]
    if default is not _static_sentinel:
        return default
    raise AttributeError(attr)


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


def formatannotation(annotation, base_module=None):
    if getattr(annotation, '__module__', None) == 'typing':
        import re
        def repl(match):
            text = match.group()
            return text.removeprefix('typing.')
        return re.sub(r'[\w\.]+', repl, repr(annotation))
    if isinstance(annotation, type):
        if annotation.__module__ in ('builtins', base_module):
            return annotation.__qualname__
        return annotation.__module__ + '.' + annotation.__qualname__
    return repr(annotation)


def unwrap(func, *, stop=None):
    """Walk the `__wrapped__` chain of *func*, returning the innermost
    callable (CPython `inspect.unwrap`, including the cycle guard)."""
    if stop is None:
        def _is_wrapper(f):
            return hasattr(f, '__wrapped__')
    else:
        def _is_wrapper(f):
            return hasattr(f, '__wrapped__') and not stop(f)
    f = func  # remember the original func for error reporting
    # Memoise by id to tolerate non-hashable objects, but store objects
    # to ensure they aren't destroyed, which would allow their IDs to be
    # reused.
    memo = {id(f): f}
    recursion_limit = sys.getrecursionlimit()
    while _is_wrapper(func):
        func = func.__wrapped__
        id_func = id(func)
        if (id_func in memo) or (len(memo) >= recursion_limit):
            raise ValueError(f'wrapper loop when unwrapping {f!r}')
        memo[id_func] = func
    return func


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
            out += f": {formatannotation(self._annotation)}"
        if self._default is not _empty:
            sep = " = " if self._annotation is not _empty else "="
            out += sep + repr(self._default)
        return out

    def __eq__(self, other):
        # CPython Parameter.__eq__: compare the (name, kind, annotation,
        # default) basis.
        if self is other:
            return True
        if not isinstance(other, Parameter):
            return NotImplemented
        return (self._name == other._name
                and self._kind == other._kind
                and self._default == other._default
                and self._annotation == other._annotation)

    def __hash__(self):
        return hash((self._name, self._kind,
                     self._default if self._default is not _empty else _empty,
                     self._annotation if self._annotation is not _empty else _empty))


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

    def _hash_basis(self):
        # CPython: keyword-only parameters compare order-insensitively.
        params = tuple(p for p in self._parameters.values()
                       if p.kind != Parameter.KEYWORD_ONLY)
        kwo_params = {p.name: p for p in self._parameters.values()
                      if p.kind == Parameter.KEYWORD_ONLY}
        return params, kwo_params, self._return_annotation

    def __eq__(self, other):
        if self is other:
            return True
        if not isinstance(other, Signature):
            return NotImplemented
        return self._hash_basis() == other._hash_basis()

    def __hash__(self):
        params, kwo_params, return_annotation = self._hash_basis()
        kwo_params = frozenset(kwo_params.values())
        return hash((params, kwo_params, return_annotation))

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

    def format(self, *, max_width=None):
        """Create a string representation of the Signature object.

        If *max_width* is passed and the one-line rendering is longer,
        every parameter goes on its own line (CPython 3.13
        `Signature.format`).
        """
        result = []
        render_pos_only_separator = False
        render_kw_only_separator = True
        for p in self._parameters.values():
            formatted = str(p)
            kind = p.kind
            if kind == Parameter.POSITIONAL_ONLY:
                render_pos_only_separator = True
            elif render_pos_only_separator:
                # We have a separator, and we've just got to a non-pos-only param.
                result.append("/")
                render_pos_only_separator = False
            if kind == Parameter.VAR_POSITIONAL:
                # OK, we have an '*args'-like parameter, so we won't need '*'.
                render_kw_only_separator = False
            elif kind == Parameter.KEYWORD_ONLY and render_kw_only_separator:
                result.append("*")
                render_kw_only_separator = False
            result.append(formatted)
        if render_pos_only_separator:
            # There were only positional-only parameters, hence the flag was
            # not reset to 'False'.
            result.append("/")
        rendered = "(" + ", ".join(result) + ")"
        if max_width is not None and len(rendered) > max_width:
            rendered = "(\n    " + ",\n    ".join(result) + "\n)"
        if self._return_annotation is not _empty:
            rendered += f" -> {formatannotation(self._return_annotation)}"
        return rendered

    def __str__(self):
        return self.format()

    @classmethod
    def from_callable(cls, func):
        return signature(func)


def _signature_drop_first(sig):
    """Remove the leading (bound) parameter — CPython drops it by
    position, not by name."""
    params = list(sig.parameters.values())[1:]
    return Signature(params, return_annotation=sig.return_annotation)


def _signature_get_partial(wrapped_sig, part, extra_args=()):
    """Signature of a `functools.partial` given its target's signature
    (mirrors CPython `_signature_get_partial`)."""
    old_params = wrapped_sig.parameters
    new_params = dict(old_params.items())
    partial_args = part.args or ()
    partial_keywords = part.keywords or {}
    if extra_args:
        partial_args = extra_args + partial_args
    try:
        ba = wrapped_sig.bind_partial(*partial_args, **partial_keywords)
    except TypeError as ex:
        raise ValueError(ex.args[0] if ex.args else str(ex)) from None

    transform_to_kwonly = False
    for param_name, param in old_params.items():
        try:
            arg_value = ba.arguments[param_name]
        except KeyError:
            pass
        else:
            if param.kind == Parameter.POSITIONAL_ONLY:
                # Bound by partial.func: disappears from the signature.
                new_params.pop(param_name)
                continue
            if param.kind == Parameter.POSITIONAL_OR_KEYWORD:
                if param_name in partial_keywords:
                    # This parameter (and everything after it) becomes
                    # keyword-only: `partial(foo, 1, b=2)` of
                    # `foo(a, b, *args, c)` is `(*, b=2, c)`.
                    transform_to_kwonly = True
                    new_params[param_name] = param.replace(default=arg_value)
                else:
                    new_params.pop(param_name)
                    continue
            if param.kind == Parameter.KEYWORD_ONLY:
                new_params[param_name] = param.replace(default=arg_value)

        if transform_to_kwonly:
            if param.kind == Parameter.VAR_POSITIONAL:
                new_params.pop(param.name, None)
            elif param.kind == Parameter.POSITIONAL_OR_KEYWORD:
                new_params[param_name] = new_params[param_name].replace(
                    kind=Parameter.KEYWORD_ONLY)

    return Signature(list(new_params.values()),
                     return_annotation=wrapped_sig.return_annotation)


def _signature_from_text(text):
    """Build a Signature from a `__text_signature__` string (a trimmed
    version of CPython's `_signature_fromstr`): handles `$module`/`$self`
    elision, `/` and `*` markers, `*args`/`**kwargs`, and literal
    defaults."""
    import ast

    inner = text.strip()
    if inner.startswith("("):
        inner = inner[1:]
    if inner.endswith(")"):
        inner = inner[:-1]
    params = []
    # Split on top-level commas (defaults can contain commas in
    # tuples/lists — rare in clinic strings, but be careful).
    parts = []
    depth = 0
    current = ""
    for ch in inner:
        if ch in "([{":
            depth += 1
        elif ch in ")]}":
            depth -= 1
        if ch == "," and depth == 0:
            parts.append(current)
            current = ""
        else:
            current += ch
    if current.strip():
        parts.append(current)

    kind = Parameter.POSITIONAL_OR_KEYWORD
    seen_slash = False
    pending = []
    for raw in parts:
        item = raw.strip()
        if not item:
            continue
        if item == "/":
            seen_slash = True
            # Everything before the slash is positional-only.
            pending = [p.replace(kind=Parameter.POSITIONAL_ONLY) for p in pending]
            continue
        if item == "*":
            kind = Parameter.KEYWORD_ONLY
            continue
        if item.startswith("**"):
            pending.append(Parameter(item[2:], Parameter.VAR_KEYWORD))
            continue
        if item.startswith("*"):
            pending.append(Parameter(item[1:], Parameter.VAR_POSITIONAL))
            kind = Parameter.KEYWORD_ONLY
            continue
        default = _empty
        name = item
        if "=" in item:
            name, _, default_text = item.partition("=")
            name = name.strip()
            default_text = default_text.strip()
            try:
                default = ast.literal_eval(default_text)
            except Exception:
                default = default_text
        if name.startswith("$"):
            # `$module` / `$self` bind implicitly; they never appear in
            # the public signature.
            continue
        pending.append(Parameter(name, kind, default=default))
    return Signature(pending)


def signature(callable_, *, follow_wrapped=True):
    if not callable(callable_):
        raise TypeError(f"{callable_!r} is not a callable object")
    obj = callable_
    if ismethod(obj):
        # Bound method: signature of the underlying function minus the
        # bound argument.
        return _signature_drop_first(signature(obj.__func__))
    # Was this function wrapped by a decorator? An explicit
    # `__signature__` anywhere on the chain stops the walk.
    if follow_wrapped:
        obj = unwrap(obj, stop=lambda f: hasattr(f, "__signature__"))
        if ismethod(obj):
            return _signature_drop_first(signature(obj.__func__))
    explicit = getattr(obj, "__signature__", None)
    if explicit is not None:
        # CPython: `__signature__` may be a string, or a callable
        # returning one (e.g. `Enum.__signature__` is a classmethod) —
        # convert before handing it back.
        sig = explicit
        if not isinstance(sig, (Signature, str)) and callable(sig):
            sig = sig()
        if isinstance(sig, str):
            sig = _signature_from_text(sig)
        if not isinstance(sig, Signature):
            raise TypeError(
                "unexpected object {!r} in __signature__ attribute".format(explicit))
        return sig
    if isinstance(obj, functools.partial):
        return _signature_get_partial(signature(obj.func), obj)
    if isclass(obj):
        # A metaclass with a custom `__call__` takes over construction
        # entirely (CPython `_signature_from_callable`): derive the
        # signature from it, or fail with ValueError when it isn't
        # introspectable (e.g. `__call__ = dict`).
        meta = type(obj)
        if meta is not type:
            meta_call = None
            for k in getattr(meta, "__mro__", ()):
                if k is type:
                    break
                if "__call__" in getattr(k, "__dict__", {}):
                    meta_call = k.__dict__["__call__"]
                    break
            if meta_call is not None:
                if isfunction(meta_call):
                    sig = signature(meta_call)
                    params = list(sig.parameters.values())[1:]
                    return Signature(params, return_annotation=sig.return_annotation)
                raise ValueError(f"no signature found for {obj!r}")
        # Prefer __new__ when it is overridden (e.g. functools.partial), then
        # fall back to __init__. A class signature carries no return annotation.
        new = getattr(obj, "__new__", None)
        if new is not None and new is not object.__new__:
            sig = signature(new)
            params = [p for name, p in sig.parameters.items() if name != "cls"]
            return Signature(params)
        init = getattr(obj, "__init__", None)
        if init is not None and init is not object.__init__:
            sig = signature(init)
            params = [p for name, p in sig.parameters.items() if name != "self"]
            return Signature(params)
        return Signature([])
    if isbuiltin(obj):
        # Builtins publish Argument-Clinic text signatures (when they
        # do at all); parse those before any generic fallback.
        text = getattr(obj, "__text_signature__", None)
        if text:
            return _signature_from_text(text)
    if not isfunction(obj):
        # A callable instance (defines __call__ on its type): derive the
        # signature from the type's __call__, dropping the bound `self`.
        call = getattr(type(obj), "__call__", None)
        if call is not None and (isfunction(call) or ismethod(call)):
            sig = signature(call)
            params = [p for name, p in sig.parameters.items() if name != "self"]
            return Signature(params, return_annotation=sig.return_annotation)
        # Best effort: return an "unknown" signature.
        return Signature([Parameter("args", Parameter.VAR_POSITIONAL),
                          Parameter("kwargs", Parameter.VAR_KEYWORD)])
    callable_ = obj
    spec = getfullargspec(callable_)
    params = []
    defaults = spec.defaults or ()
    n_defaults = len(defaults)
    n_args = len(spec.args)
    f = _func_of(callable_)
    posonly = getattr(f.__code__, "co_posonlyargcount", 0) if f is not None else 0
    for i, name in enumerate(spec.args):
        if i >= n_args - n_defaults:
            default = defaults[i - (n_args - n_defaults)]
        else:
            default = _empty
        annotation = spec.annotations.get(name, _empty)
        kind = Parameter.POSITIONAL_ONLY if i < posonly else Parameter.POSITIONAL_OR_KEYWORD
        params.append(Parameter(name, kind,
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
