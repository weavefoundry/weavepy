"""``bdb`` — generic Python debugger base class.

Concrete debuggers (``pdb``) subclass :class:`Bdb` and override
``user_line``, ``user_call``, ``user_return``, and
``user_exception``. ``Bdb`` manages the per-file breakpoint table
and the trace-function hook.

This implementation tracks CPython's ``Lib/bdb.py`` surface for the
methods most pdb commands reach for. The deep loop-control corners
(``runeval``, ``runcall``, multi-thread tracing) are approximate.
"""

import os
import sys

__all__ = ['Bdb', 'Breakpoint', 'BdbQuit', 'GENERATOR_AND_COROUTINE_FLAGS',
            'set_trace', 'effective', 'checkfuncname']


GENERATOR_AND_COROUTINE_FLAGS = 0x20 | 0x100 | 0x200


class BdbQuit(Exception):
    """Raised when the user quits the debugger."""


class Breakpoint:
    """Represent a single source-line breakpoint."""

    next = 1
    bplist = {}  # (file, line) -> [Breakpoint]
    bpbynumber = [None]

    def __init__(self, file, line, temporary=False, cond=None,
                  funcname=None):
        self.file = file
        self.line = line
        self.temporary = temporary
        self.cond = cond
        self.funcname = funcname
        self.enabled = True
        self.ignore = 0
        self.hits = 0
        self.number = Breakpoint.next
        Breakpoint.next += 1
        Breakpoint.bpbynumber.append(self)
        Breakpoint.bplist.setdefault((file, line), []).append(self)

    def deleteMe(self):
        Breakpoint.bplist[(self.file, self.line)].remove(self)
        if not Breakpoint.bplist[(self.file, self.line)]:
            del Breakpoint.bplist[(self.file, self.line)]
        Breakpoint.bpbynumber[self.number] = None

    def enable(self):
        self.enabled = True

    def disable(self):
        self.enabled = False

    def bpprint(self, out=None):
        out = out or sys.stdout
        out.write('{} breakpoint  keep {}  at {}:{}\n'.format(
            self.number, 'yes' if self.enabled else 'no',
            self.file, self.line))
        if self.cond:
            out.write('\tstop only if {}\n'.format(self.cond))
        if self.ignore:
            out.write('\tignore next {} hits\n'.format(self.ignore))
        if self.hits:
            out.write('\tbreakpoint already hit {} times\n'.format(self.hits))

    def __str__(self):
        return 'breakpoint {} at {}:{}'.format(
            self.number, self.file, self.line)


def checkfuncname(b, frame):
    if not b.funcname:
        return True
    if frame.f_code.co_name != b.funcname:
        return False
    return frame.f_code.co_firstlineno == b.line


def effective(file, line, frame):
    """Identify the active breakpoint at ``(file, line)`` for ``frame``."""
    possibles = Breakpoint.bplist.get((file, line), [])
    for b in possibles:
        if not b.enabled:
            continue
        if not checkfuncname(b, frame):
            continue
        b.hits += 1
        if b.cond:
            try:
                ok = eval(b.cond, frame.f_globals, frame.f_locals)
            except Exception:
                return b, False
            if not ok:
                continue
        if b.ignore > 0:
            b.ignore -= 1
            continue
        return b, True
    return None, False


class Bdb:
    """Generic Python debugger base."""

    def __init__(self, skip=None):
        self.skip = set(skip) if skip else None
        self.breaks = {}      # filename -> set of linenos
        self.fncache = {}
        self.frame_returning = None
        self.botframe = None
        self.stopframe = None
        self.returnframe = None
        self.quitting = False
        self.stoplineno = 0

    # ---- canonicalisation ------------------------------------------------

    def canonic(self, filename):
        if not filename:
            return filename
        if filename == '<' + filename[1:-1] + '>':
            return filename
        canonic = self.fncache.get(filename)
        if canonic is None:
            canonic = os.path.abspath(filename)
            canonic = os.path.normcase(canonic)
            self.fncache[filename] = canonic
        return canonic

    # ---- trace dispatch -------------------------------------------------

    def reset(self):
        import linecache
        linecache.checkcache()
        self.botframe = None
        self.stopframe = None
        self.returnframe = None
        self.quitting = False

    def trace_dispatch(self, frame, event, arg):
        if self.quitting:
            return None
        if event == 'line':
            return self.dispatch_line(frame)
        if event == 'call':
            return self.dispatch_call(frame, arg)
        if event == 'return':
            return self.dispatch_return(frame, arg)
        if event == 'exception':
            return self.dispatch_exception(frame, arg)
        if event == 'opcode':
            return self.trace_dispatch
        return self.trace_dispatch

    def dispatch_line(self, frame):
        if self.stop_here(frame) or self.break_here(frame):
            self.user_line(frame)
            if self.quitting:
                raise BdbQuit
        return self.trace_dispatch

    def dispatch_call(self, frame, arg):
        if self.botframe is None:
            self.botframe = frame.f_back
            return self.trace_dispatch
        if not self.stop_here(frame) and not self.break_anywhere(frame):
            return None
        self.user_call(frame, arg)
        if self.quitting:
            raise BdbQuit
        return self.trace_dispatch

    def dispatch_return(self, frame, arg):
        if self.stop_here(frame) or frame == self.returnframe:
            try:
                self.frame_returning = frame
                self.user_return(frame, arg)
            finally:
                self.frame_returning = None
            if self.quitting:
                raise BdbQuit
        return self.trace_dispatch

    def dispatch_exception(self, frame, arg):
        if self.stop_here(frame):
            self.user_exception(frame, arg)
            if self.quitting:
                raise BdbQuit
        return self.trace_dispatch

    # ---- stop checks -----------------------------------------------------

    def stop_here(self, frame):
        if self.skip and self.is_skipped_module(frame.f_globals.get('__name__')):
            return False
        if frame is self.stopframe:
            if self.stoplineno == -1:
                return False
            return frame.f_lineno >= self.stoplineno
        if not self.stopframe:
            return True
        return False

    def break_here(self, frame):
        filename = self.canonic(frame.f_code.co_filename)
        if filename not in self.breaks:
            return False
        lineno = frame.f_lineno
        if lineno not in self.breaks[filename]:
            return False
        bp, found = effective(filename, lineno, frame)
        if not found:
            return False
        self.currentbp = bp.number
        if bp.temporary:
            self.do_clear(str(bp.number))
        return True

    def break_anywhere(self, frame):
        filename = self.canonic(frame.f_code.co_filename)
        return filename in self.breaks

    def is_skipped_module(self, module_name):
        return module_name in self.skip if self.skip and module_name else False

    # ---- user hooks (override in subclasses) -----------------------------

    def user_call(self, frame, argument_list):
        pass

    def user_line(self, frame):
        pass

    def user_return(self, frame, return_value):
        pass

    def user_exception(self, frame, exc_info):
        pass

    # ---- step / continue -------------------------------------------------

    def _set_stopinfo(self, stopframe, returnframe, stoplineno=0):
        self.stopframe = stopframe
        self.returnframe = returnframe
        self.quitting = False
        self.stoplineno = stoplineno

    def set_until(self, frame, lineno=None):
        if lineno is None:
            lineno = frame.f_lineno + 1
        self._set_stopinfo(frame, frame, lineno)

    def set_step(self):
        self._set_stopinfo(None, None)

    def set_next(self, frame):
        self._set_stopinfo(frame, None)

    def set_return(self, frame):
        self._set_stopinfo(frame.f_back, frame)

    def set_trace(self, frame=None):
        if frame is None:
            frame = sys._getframe().f_back
        self.reset()
        while frame:
            frame.f_trace = self.trace_dispatch
            self.botframe = frame
            frame = frame.f_back
        self.set_step()
        sys.settrace(self.trace_dispatch)

    def set_continue(self):
        self._set_stopinfo(self.botframe, None, -1)

    def set_quit(self):
        self.stopframe = self.botframe
        self.returnframe = None
        self.quitting = True
        sys.settrace(None)

    # ---- breakpoints -----------------------------------------------------

    def set_break(self, filename, lineno, temporary=False, cond=None,
                    funcname=None):
        filename = self.canonic(filename)
        self.breaks.setdefault(filename, set()).add(lineno)
        Breakpoint(filename, lineno, temporary, cond, funcname)

    def clear_break(self, filename, lineno):
        filename = self.canonic(filename)
        if filename not in self.breaks:
            return 'no breakpoints at {}:{}'.format(filename, lineno)
        if lineno not in self.breaks[filename]:
            return 'no breakpoint at {}:{}'.format(filename, lineno)
        for bp in Breakpoint.bplist.get((filename, lineno), [])[:]:
            bp.deleteMe()
        if not Breakpoint.bplist.get((filename, lineno)):
            self.breaks[filename].discard(lineno)
            if not self.breaks[filename]:
                del self.breaks[filename]
        return None

    def clear_all_breaks(self):
        for filename in list(self.breaks):
            for lineno in list(self.breaks[filename]):
                self.clear_break(filename, lineno)
        self.breaks.clear()

    def get_breaks(self, filename, lineno):
        filename = self.canonic(filename)
        return Breakpoint.bplist.get((filename, lineno), [])

    def get_file_breaks(self, filename):
        return list(self.breaks.get(self.canonic(filename), []))

    def get_all_breaks(self):
        return [(f, l) for f, ls in self.breaks.items() for l in ls]

    def do_clear(self, arg):
        """Default implementation — pdb overrides this."""
        try:
            num = int(arg)
        except ValueError:
            return
        if 0 < num < len(Breakpoint.bpbynumber):
            bp = Breakpoint.bpbynumber[num]
            if bp is not None:
                bp.deleteMe()

    # ---- run helpers -----------------------------------------------------

    def runcall(self, func, *args, **kwds):
        self.reset()
        sys.settrace(self.trace_dispatch)
        try:
            return func(*args, **kwds)
        finally:
            sys.settrace(None)

    def run(self, cmd, globals=None, locals=None):
        if globals is None:
            globals = {'__name__': '__main__'}
        if locals is None:
            locals = globals
        self.reset()
        if isinstance(cmd, str):
            cmd = compile(cmd, '<string>', 'exec')
        sys.settrace(self.trace_dispatch)
        try:
            exec(cmd, globals, locals)
        except BdbQuit:
            pass
        finally:
            self.quitting = True
            sys.settrace(None)


def set_trace():
    """Standalone helper — drop into the default pdb."""
    import pdb
    pdb.set_trace()
