"""WeavePy `traceback` — formatting and walking of tracebacks.

Mirrors the CPython API. Internally relies on `tb_frame`, `tb_next`,
`tb_lineno` (real traceback objects from RFC 0018) plus
`__cause__` / `__context__` / `__suppress_context__` to walk
exception chains.
"""

import sys
import linecache


__all__ = [
    "extract_stack",
    "extract_tb",
    "format_exc",
    "format_exception",
    "format_exception_only",
    "format_list",
    "format_stack",
    "format_tb",
    "print_exc",
    "print_exception",
    "print_last",
    "print_stack",
    "print_tb",
    "walk_stack",
    "walk_tb",
    "StackSummary",
    "FrameSummary",
    "TracebackException",
    "clear_frames",
]


def format_exc(limit=None, chain=True):
    typ, val, tb = sys.exc_info()
    if val is None:
        return "None\n"
    return "".join(format_exception(typ, val, tb, limit=limit, chain=chain))


_RECURSIVE_CUTOFF = 3


class FrameSummary:
    """One frame in a captured traceback."""

    __slots__ = ("filename", "lineno", "name", "_line", "locals", "end_lineno",
                 "colno", "end_colno")

    def __init__(self, filename, lineno, name, *, lookup_line=True, locals=None,
                 line=None, end_lineno=None, colno=None, end_colno=None):
        self.filename = filename
        self.lineno = lineno
        self.name = name
        self._line = line
        self.locals = {k: repr(v) for k, v in locals.items()} if locals is not None else None
        self.end_lineno = end_lineno if end_lineno is not None else lineno
        self.colno = colno
        self.end_colno = end_colno
        if lookup_line:
            self.line  # touch the property

    @property
    def line(self):
        if self._line is None and self.filename and self.lineno is not None:
            self._line = linecache.getline(self.filename, self.lineno).strip()
        return self._line

    def __eq__(self, other):
        if isinstance(other, FrameSummary):
            return (
                self.filename == other.filename
                and self.lineno == other.lineno
                and self.name == other.name
                and self.locals == other.locals
            )
        if isinstance(other, tuple):
            return (self.filename, self.lineno, self.name, self.line) == other
        return NotImplemented

    def __getitem__(self, pos):
        return (self.filename, self.lineno, self.name, self.line)[pos]

    def __iter__(self):
        return iter([self.filename, self.lineno, self.name, self.line])

    def __repr__(self):
        return f"<FrameSummary file {self.filename}, line {self.lineno} in {self.name}>"

    def __len__(self):
        return 4


def walk_stack(f):
    if f is None:
        try:
            f = sys._getframe().f_back.f_back
        except Exception:
            return
    while f is not None:
        yield f, f.f_lineno
        f = f.f_back


def walk_tb(tb):
    while tb is not None:
        yield tb.tb_frame, tb.tb_lineno
        tb = tb.tb_next


def _get_code_position(code, instruction_index):
    """PEP-657 (lineno, end_lineno, colno, end_colno) for a bytecode offset.

    `co_positions()` yields one tuple per *code unit* (2 bytes), so the
    instruction byte offset maps to entry `instruction_index // 2`.
    """
    if instruction_index is None or instruction_index < 0:
        return (None, None, None, None)
    try:
        positions = list(code.co_positions())
    except Exception:
        return (None, None, None, None)
    idx = instruction_index // 2
    if 0 <= idx < len(positions):
        return positions[idx]
    return (None, None, None, None)


def _walk_tb_with_full_positions(tb):
    # Like walk_tb, but yields full code positions (end line + columns).
    while tb is not None:
        positions = _get_code_position(tb.tb_frame.f_code, tb.tb_lasti)
        # Fall back to tb_lineno when co_positions has no line, matching
        # walk_tb's behavior.
        if positions[0] is None:
            yield tb.tb_frame, (tb.tb_lineno,) + tuple(positions[1:])
        else:
            yield tb.tb_frame, positions
        tb = tb.tb_next


class StackSummary:
    """A sequence of FrameSummary objects with extra formatting helpers.

    Note: in CPython this is a list subclass, but WeavePy's `list` does
    not yet support subclassing with full method inheritance, so we
    implement the necessary parts via composition.
    """

    def __init__(self, frames=None):
        self._frames = list(frames) if frames else []

    def append(self, frame):
        self._frames.append(frame)

    def extend(self, frames):
        self._frames.extend(frames)

    def insert(self, index, frame):
        self._frames.insert(index, frame)

    def pop(self, index=-1):
        return self._frames.pop(index)

    def remove(self, frame):
        self._frames.remove(frame)

    def reverse(self):
        self._frames.reverse()

    def count(self, frame):
        return self._frames.count(frame)

    def index(self, frame, *args):
        return self._frames.index(frame, *args)

    def __len__(self):
        return len(self._frames)

    def __iter__(self):
        return iter(self._frames)

    def __reversed__(self):
        return reversed(self._frames)

    def __contains__(self, frame):
        return frame in self._frames

    def __getitem__(self, index):
        return self._frames[index]

    def __setitem__(self, index, value):
        self._frames[index] = value

    def __delitem__(self, index):
        del self._frames[index]

    def __bool__(self):
        return bool(self._frames)

    def __eq__(self, other):
        if isinstance(other, StackSummary):
            return self._frames == other._frames
        if isinstance(other, list):
            return self._frames == other
        return NotImplemented

    @classmethod
    def extract(cls, frame_gen, *, limit=None, lookup_lines=True, capture_locals=False):
        # `frame_gen` yields plain (frame, lineno) pairs (no column info).
        # Adapt to the extended generator the position-aware path consumes.
        def extended_frame_gen():
            for f, lineno in frame_gen:
                yield f, (lineno, None, None, None)

        return cls._extract_from_extended_frame_gen(
            extended_frame_gen(), limit=limit, lookup_lines=lookup_lines,
            capture_locals=capture_locals)

    @classmethod
    def _extract_from_extended_frame_gen(cls, frame_gen, *, limit=None,
                                         lookup_lines=True, capture_locals=False):
        # Like `extract`, but consumes (frame, (lineno, end_lineno, colno,
        # end_colno)) tuples so PEP-657 column anchors survive into each
        # FrameSummary. Only lineno is required; the rest may be None.
        if limit is None:
            limit = getattr(sys, "tracebacklimit", None)
        if isinstance(limit, int) and limit < 0:
            limit = 0
        result = cls()
        frames = list(frame_gen)
        if isinstance(limit, int):
            frames = frames[-limit:] if limit else []
        for f, (lineno, end_lineno, colno, end_colno) in frames:
            try:
                co = f.f_code
                filename = getattr(co, "co_filename", "<unknown>")
                name = getattr(co, "co_name", "<unknown>")
            except Exception:
                filename = "<unknown>"
                name = "<unknown>"
            locals_ = f.f_locals if capture_locals else None
            result.append(
                FrameSummary(filename, lineno, name, lookup_line=lookup_lines,
                             locals=locals_, end_lineno=end_lineno,
                             colno=colno, end_colno=end_colno)
            )
        return result

    @classmethod
    def from_list(cls, a_list):
        result = cls()
        for item in a_list:
            if isinstance(item, FrameSummary):
                result.append(item)
            else:
                filename, lineno, name, line = item
                f = FrameSummary(filename, lineno, name, lookup_line=False)
                f._line = line
                result.append(f)
        return result

    def format(self):
        result = []
        last_name = None
        last_file = None
        count = 0
        for frame in self:
            if (last_name == frame.name and last_file == frame.filename):
                count += 1
                if count > _RECURSIVE_CUTOFF:
                    continue
            else:
                if count > _RECURSIVE_CUTOFF:
                    result.append(
                        f"  [Previous line repeated {count - _RECURSIVE_CUTOFF} more times]\n"
                    )
                last_name = frame.name
                last_file = frame.filename
                count = 1
            line_repr = f'  File "{frame.filename}", line {frame.lineno}, in {frame.name}\n'
            if frame.line:
                line_repr += f"    {frame.line.strip()}\n"
            if frame.locals:
                for k in sorted(frame.locals):
                    line_repr += f"    {k} = {frame.locals[k]}\n"
            result.append(line_repr)
        if count > _RECURSIVE_CUTOFF:
            result.append(
                f"  [Previous line repeated {count - _RECURSIVE_CUTOFF} more times]\n"
            )
        return result

    def format_frame_summary(self, frame_summary):
        return self.from_list([frame_summary]).format()[0]


def extract_tb(tb, limit=None):
    return StackSummary._extract_from_extended_frame_gen(
        _walk_tb_with_full_positions(tb), limit=limit)


def extract_stack(f=None, limit=None):
    if f is None:
        try:
            f = sys._getframe().f_back
        except Exception:
            return StackSummary()
    stack = StackSummary.extract(walk_stack(f), limit=limit)
    stack.reverse()
    return stack


def format_list(extracted_list):
    return StackSummary.from_list(extracted_list).format()


def format_tb(tb, limit=None):
    return format_list(extract_tb(tb, limit=limit))


def format_stack(f=None, limit=None):
    return format_list(extract_stack(f, limit=limit))


def format_exception_only(exc, value=None):
    if value is None and isinstance(exc, BaseException):
        value = exc
    if value is None:
        return [f"{getattr(exc, '__name__', repr(exc))}\n"]
    cls = type(value)
    name = getattr(cls, "__qualname__", cls.__name__)
    mod = getattr(cls, "__module__", None)
    if mod is not None and mod not in ("builtins", "__main__"):
        full = f"{mod}.{name}"
    else:
        full = name
    text = str(value)
    if text:
        return [f"{full}: {text}\n"]
    return [f"{full}\n"]


def format_exception(exc, value=None, tb=None, limit=None, chain=True):
    if value is None and isinstance(exc, BaseException):
        value = exc
        tb = exc.__traceback__
    te = TracebackException(type(value) if value is not None else exc,
                            value, tb, limit=limit, capture_locals=False, compact=False)
    return list(te.format(chain=chain))


def print_tb(tb, limit=None, file=None):
    if file is None:
        file = sys.stderr
    for line in format_tb(tb, limit=limit):
        file.write(line)


def print_exception(exc, value=None, tb=None, limit=None, file=None, chain=True):
    if value is None and isinstance(exc, BaseException):
        value = exc
        tb = exc.__traceback__
    if file is None:
        file = sys.stderr
    for line in format_exception(type(value) if value is not None else exc,
                                  value, tb, limit=limit, chain=chain):
        file.write(line)


def print_exc(limit=None, file=None, chain=True):
    typ, val, tb = sys.exc_info()
    print_exception(typ, val, tb, limit=limit, file=file, chain=chain)


def print_last(limit=None, file=None, chain=True):
    if not hasattr(sys, "last_exc"):
        raise ValueError("no last exception")
    print_exception(type(sys.last_exc), sys.last_exc,
                    sys.last_exc.__traceback__, limit=limit, file=file, chain=chain)


def print_stack(f=None, limit=None, file=None):
    if file is None:
        file = sys.stderr
    for line in format_stack(f=f, limit=limit):
        file.write(line)


def clear_frames(tb):
    while tb is not None:
        try:
            tb.tb_frame.clear()
        except Exception:
            pass
        tb = tb.tb_next


# ----------- TracebackException ----------- #

class TracebackException:
    """Captured exception state suitable for offline formatting."""

    def __init__(self, exc_type, exc_value, exc_tb, *, limit=None,
                 lookup_lines=True, capture_locals=False, compact=False,
                 max_group_width=15, max_group_depth=10, _seen=None):
        self.exc_type = exc_type
        self._str = str(exc_value) if exc_value is not None else ""
        self.stack = StackSummary._extract_from_extended_frame_gen(
            _walk_tb_with_full_positions(exc_tb), limit=limit,
            lookup_lines=lookup_lines, capture_locals=capture_locals)
        self.filename = getattr(exc_value, "filename", None)
        self.lineno = getattr(exc_value, "lineno", None)
        self.text = getattr(exc_value, "text", None)
        self.offset = getattr(exc_value, "offset", None)
        self.msg = getattr(exc_value, "msg", None)
        self.exceptions = None
        self.__suppress_context__ = bool(getattr(exc_value, "__suppress_context__", False))
        self.__notes__ = getattr(exc_value, "__notes__", None)
        self.__cause__ = None
        self.__context__ = None
        if _seen is None:
            _seen = set()
        _seen.add(id(exc_value))
        cause = getattr(exc_value, "__cause__", None)
        if cause is not None and id(cause) not in _seen:
            self.__cause__ = TracebackException(
                type(cause), cause, cause.__traceback__,
                limit=limit, lookup_lines=lookup_lines, capture_locals=capture_locals,
                _seen=_seen,
            )
        context = getattr(exc_value, "__context__", None)
        if context is not None and id(context) not in _seen:
            self.__context__ = TracebackException(
                type(context), context, context.__traceback__,
                limit=limit, lookup_lines=lookup_lines, capture_locals=capture_locals,
                _seen=_seen,
            )
        excs = getattr(exc_value, "exceptions", None)
        if excs is not None and hasattr(exc_value, "split"):
            self.exceptions = [
                TracebackException(type(e), e, e.__traceback__,
                                   limit=limit, lookup_lines=lookup_lines,
                                   capture_locals=capture_locals, _seen=_seen)
                for e in excs[:max_group_width]
            ]

    @classmethod
    def from_exception(cls, exc, **kwargs):
        return cls(type(exc), exc, exc.__traceback__, **kwargs)

    def format(self, *, chain=True):
        if chain:
            if self.__cause__ is not None:
                yield from self.__cause__.format(chain=chain)
                yield "\nThe above exception was the direct cause of the following exception:\n\n"
            elif self.__context__ is not None and not self.__suppress_context__:
                yield from self.__context__.format(chain=chain)
                yield "\nDuring handling of the above exception, another exception occurred:\n\n"
        if self.stack:
            yield "Traceback (most recent call last):\n"
            yield from self.stack.format()
        yield from self.format_exception_only()
        if self.exceptions:
            for i, te in enumerate(self.exceptions):
                yield "\n--+---------------- " + str(i + 1) + " ----------------\n"
                yield from te.format(chain=chain)

    def format_exception_only(self):
        cls = self.exc_type
        if cls is None:
            yield "None\n"
            return
        name = getattr(cls, "__qualname__", cls.__name__)
        mod = getattr(cls, "__module__", None)
        if mod is not None and mod not in ("builtins", "__main__"):
            full = f"{mod}.{name}"
        else:
            full = name
        if self._str:
            yield f"{full}: {self._str}\n"
        else:
            yield f"{full}\n"
        if self.__notes__:
            for note in self.__notes__:
                yield str(note) + "\n"
