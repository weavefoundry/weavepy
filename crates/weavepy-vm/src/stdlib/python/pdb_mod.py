"""``pdb`` — the canonical interactive Python debugger.

WeavePy's ``pdb`` is a port of CPython's ``Lib/pdb.py``, scoped to
the command set users actually reach for and the surface the
ecosystem (pytest, ipdb, devtools) hooks into.

Quick reference (the typical session):

    pdb.set_trace()          # set a breakpoint at this line
    python -m pdb script.py  # run script under pdb
    pdb.post_mortem(tb)      # debug a traceback after the fact

Commands at the (Pdb) prompt:

    h / help [topic]     help
    s / step             step into
    n / next             step over
    r / return           run until return
    c / continue         continue execution
    q / quit             quit debugger
    b / break [arg]      set / list breakpoints
    cl / clear [arg]     clear breakpoints
    disable / enable     toggle breakpoints
    where / w / bt       backtrace
    u / up [n]           move up the stack
    d / down [n]         move down the stack
    l / list             list source
    ll / longlist        list whole function
    p expr               print
    pp expr              pretty-print
    a / args             show arg values
    retval               last return value
    unt / until [line]   continue to a line
    j / jump line        jump to line
    debug stmt           recursive debug
    display              watch expressions
    condition n cond     conditional break
    commands [n]         attach commands to a breakpoint
    ignore n count       skip N hits
    alias / unalias      shorthand
    EOF                  quit
"""

import bdb
import cmd
import linecache
import os
import pprint
import re
import sys


__all__ = ['set_trace', 'post_mortem', 'pm', 'run', 'runeval',
            'runctx', 'runcall', 'Pdb']


class Restart(Exception):
    pass


class Pdb(bdb.Bdb, cmd.Cmd):
    """The interactive debugger."""

    prompt = '(Pdb) '
    identchars = cmd.Cmd.identchars + '%'
    rcLines = []
    commands = {}
    commands_resuming = {'do_continue', 'do_step', 'do_next',
                          'do_return', 'do_quit', 'do_jump'}

    def __init__(self, completekey='tab', stdin=None, stdout=None,
                  skip=None, nosigint=False, readrc=True):
        bdb.Bdb.__init__(self, skip)
        cmd.Cmd.__init__(self, completekey, stdin, stdout)
        self.aliases = {}
        self.displaying = {}
        self.mainpyfile = ''
        self._wait_for_mainpyfile = False
        self.tb_lineno = {}
        self.commands_doprompt = {}
        self.commands_silent = {}
        self.commands_defining = False
        self.commands_bnum = None
        self.nosigint = nosigint
        self.curframe = None
        self.stack = []
        self.curindex = 0
        self.lineno = None
        self.message = self._message
        self.error = self._error
        if readrc:
            for env in ('PDBRC', None):
                if env is None:
                    home = os.path.expanduser('~')
                    fname = os.path.join(home, '.pdbrc')
                elif env in os.environ:
                    fname = os.environ[env]
                else:
                    continue
                try:
                    with open(fname) as f:
                        self.rcLines.extend(f.readlines())
                except OSError:
                    pass

    # ---- output helpers -------------------------------------------------

    def _message(self, msg):
        print(msg, file=self.stdout)

    def _error(self, msg):
        print('***', msg, file=self.stdout)

    # ---- hooks from Bdb -------------------------------------------------

    def user_call(self, frame, args):
        if self._wait_for_mainpyfile:
            return
        name = frame.f_code.co_name or '?'
        self.message('--Call--')
        self.interaction(frame, None)

    def user_line(self, frame):
        if self._wait_for_mainpyfile:
            if (self.mainpyfile != self.canonic(frame.f_code.co_filename)
                or frame.f_lineno <= 0):
                return
            self._wait_for_mainpyfile = False
        if self.bp_commands(frame):
            self.interaction(frame, None)

    def bp_commands(self, frame):
        if getattr(self, 'currentbp', False) and \
                self.currentbp in self.commands:
            commands = self.commands[self.currentbp]
            doprompt = self.commands_doprompt.get(self.currentbp, True)
            silent = self.commands_silent.get(self.currentbp, False)
            saved = self.cmdqueue
            self.cmdqueue = list(commands)
            self.allow_kbdint = True
            self.cmdloop()
            self.allow_kbdint = False
            self.cmdqueue = saved
            del self.currentbp
            return doprompt
        return True

    def user_return(self, frame, retval):
        if self._wait_for_mainpyfile:
            return
        frame.f_locals['__return__'] = retval
        self.message('--Return--')
        self.interaction(frame, None)

    def user_exception(self, frame, exc_info):
        if self._wait_for_mainpyfile:
            return
        exc_type, exc_value, exc_traceback = exc_info
        frame.f_locals['__exception__'] = exc_type, exc_value
        self.message('{}: {}'.format(getattr(exc_type, '__name__', exc_type),
                                       exc_value))
        self.interaction(frame, exc_traceback)

    # ---- interaction loop -----------------------------------------------

    def interaction(self, frame, traceback):
        self.setup(frame, traceback)
        self.print_stack_entry(self.stack[self.curindex])
        self._cmdloop()
        self.forget()

    def _cmdloop(self):
        try:
            self.cmdloop()
        except KeyboardInterrupt:
            self.message('--KeyboardInterrupt--')

    def displayhook(self, obj):
        if obj is not None:
            self.message(repr(obj))

    def setup(self, f, tb):
        self.forget()
        self.stack, self.curindex = self.get_stack(f, tb)
        while tb:
            lineno = self.lineno_from_tb(tb)
            self.tb_lineno[tb.tb_frame] = lineno
            tb = tb.tb_next
        self.curframe = self.stack[self.curindex][0]
        self.curframe_locals = self.curframe.f_locals

    def forget(self):
        self.lineno = None
        self.stack = []
        self.curindex = 0
        self.curframe = None
        self.tb_lineno.clear()

    def get_stack(self, f, t):
        stack = []
        if t and t.tb_frame is f:
            t = t.tb_next
        while f is not None:
            stack.append((f, f.f_lineno))
            if f is self.botframe:
                break
            f = f.f_back
        stack.reverse()
        i = max(0, len(stack) - 1)
        while t is not None:
            stack.append((t.tb_frame, self.lineno_from_tb(t)))
            t = t.tb_next
        if f is None:
            i = max(0, len(stack) - 1)
        return stack, i

    def lineno_from_tb(self, tb):
        return getattr(tb, 'tb_lineno', None) or 0

    # ---- printing -------------------------------------------------------

    def print_stack_entry(self, frame_lineno, prompt_prefix='\n-> '):
        frame, lineno = frame_lineno
        if frame is self.curframe:
            prefix = '> '
        else:
            prefix = '  '
        self.message('{}{}'.format(prefix, self.format_stack_entry(
            frame_lineno, prompt_prefix)))

    def format_stack_entry(self, frame_lineno, lprefix=': '):
        frame, lineno = frame_lineno
        filename = self.canonic(frame.f_code.co_filename)
        s = '{}({})'.format(filename, lineno)
        if frame.f_code.co_name:
            s += frame.f_code.co_name
        else:
            s += '<lambda>'
        s += '()'
        if '__return__' in frame.f_locals:
            rv = frame.f_locals['__return__']
            s += '->{}'.format(repr(rv))
        line = linecache.getline(filename, lineno, frame.f_globals)
        if line:
            s += lprefix + line.strip()
        return s

    def print_stack_trace(self):
        try:
            for frame_lineno in self.stack:
                self.print_stack_entry(frame_lineno)
        except KeyboardInterrupt:
            pass

    # ---- commands -------------------------------------------------------

    def default(self, line):
        if line.startswith('!'):
            line = line[1:]
        locals = self.curframe_locals
        try:
            obj = eval(line, self.curframe.f_globals, locals)
            self.displayhook(obj)
        except Exception:
            t, v = sys.exc_info()[:2]
            self.error('{}: {}'.format(getattr(t, '__name__', t), v))

    def precmd(self, line):
        if line == 'EOF':
            return 'quit'
        return line

    def do_help(self, arg):
        if not arg:
            self.message(HELP_TEXT)
            return
        try:
            doc = getattr(self, 'do_' + arg).__doc__
        except AttributeError:
            doc = None
        if doc:
            self.message(doc.strip())
        else:
            self.message('No help for {!r}'.format(arg))
    do_h = do_help

    def do_break(self, arg):
        if not arg:
            for bp in bdb.Breakpoint.bpbynumber:
                if bp:
                    bp.bpprint(self.stdout)
            return
        filename, lineno = self._parse_break(arg)
        if filename is None:
            self.error('bad break spec: {!r}'.format(arg))
            return
        self.set_break(filename, lineno)
        self.message('Breakpoint set at {}:{}'.format(filename, lineno))
    do_b = do_break

    def _parse_break(self, arg):
        if ':' in arg:
            f, _, l = arg.partition(':')
            try:
                return f, int(l)
            except ValueError:
                return None, None
        try:
            return self.curframe.f_code.co_filename, int(arg)
        except ValueError:
            return None, None

    def do_clear(self, arg):
        if not arg:
            self.message('clear all breakpoints')
            bdb.Bdb.clear_all_breaks(self)
            return
        try:
            num = int(arg)
        except ValueError:
            self.error('clear requires a breakpoint number')
            return
        bp = bdb.Breakpoint.bpbynumber[num]
        if bp:
            bp.deleteMe()
    do_cl = do_clear

    def do_disable(self, arg):
        try:
            num = int(arg)
        except ValueError:
            self.error('disable requires a breakpoint number')
            return
        bp = bdb.Breakpoint.bpbynumber[num]
        if bp:
            bp.disable()

    def do_enable(self, arg):
        try:
            num = int(arg)
        except ValueError:
            self.error('enable requires a breakpoint number')
            return
        bp = bdb.Breakpoint.bpbynumber[num]
        if bp:
            bp.enable()

    def do_step(self, arg):
        self.set_step()
        return 1
    do_s = do_step

    def do_next(self, arg):
        self.set_next(self.curframe)
        return 1
    do_n = do_next

    def do_return(self, arg):
        self.set_return(self.curframe)
        return 1
    do_r = do_return

    def do_continue(self, arg):
        self.set_continue()
        return 1
    do_c = do_cont = do_continue

    def do_quit(self, arg):
        self._user_requested_quit = True
        self.set_quit()
        return 1
    do_q = do_exit = do_quit

    def do_where(self, arg):
        self.print_stack_trace()
    do_w = do_bt = do_where

    def do_up(self, arg):
        try:
            count = int(arg) if arg else 1
        except ValueError:
            self.error('up takes an integer count')
            return
        if self.curindex - count < 0:
            self.error('Oldest frame')
            return
        self.curindex -= count
        self.curframe = self.stack[self.curindex][0]
        self.curframe_locals = self.curframe.f_locals
        self.print_stack_entry(self.stack[self.curindex])
    do_u = do_up

    def do_down(self, arg):
        try:
            count = int(arg) if arg else 1
        except ValueError:
            self.error('down takes an integer count')
            return
        if self.curindex + count >= len(self.stack):
            self.error('Newest frame')
            return
        self.curindex += count
        self.curframe = self.stack[self.curindex][0]
        self.curframe_locals = self.curframe.f_locals
        self.print_stack_entry(self.stack[self.curindex])
    do_d = do_down

    def do_list(self, arg):
        filename = self.curframe.f_code.co_filename
        breaklist = self.get_file_breaks(filename)
        try:
            lineno = self.curframe.f_lineno - 5
            for offset in range(11):
                line = linecache.getline(filename, lineno + offset,
                                          self.curframe.f_globals)
                if not line:
                    break
                marker = '->' if (lineno + offset) == self.curframe.f_lineno else '  '
                bp = 'B' if (lineno + offset) in breaklist else ' '
                self.message('{:4d}{}{} {}'.format(
                    lineno + offset, bp, marker, line.rstrip()))
        except Exception as exc:
            self.error(str(exc))
    do_l = do_list

    def do_longlist(self, arg):
        filename = self.curframe.f_code.co_filename
        lineno = self.curframe.f_code.co_firstlineno or 1
        while True:
            line = linecache.getline(filename, lineno,
                                      self.curframe.f_globals)
            if not line:
                break
            marker = '->' if lineno == self.curframe.f_lineno else '  '
            self.message('{:4d} {} {}'.format(lineno, marker, line.rstrip()))
            lineno += 1
    do_ll = do_longlist

    def do_print(self, arg):
        try:
            self.displayhook(eval(arg, self.curframe.f_globals,
                                   self.curframe_locals))
        except Exception as exc:
            self.error(str(exc))
    do_p = do_print

    def do_pp(self, arg):
        try:
            value = eval(arg, self.curframe.f_globals, self.curframe_locals)
            pprint.pprint(value, stream=self.stdout)
        except Exception as exc:
            self.error(str(exc))

    def do_args(self, arg):
        code = self.curframe.f_code
        names = code.co_varnames[:code.co_argcount + getattr(code, 'co_kwonlyargcount', 0)]
        for n in names:
            self.message('{} = {!r}'.format(n, self.curframe_locals.get(n)))
    do_a = do_args

    def do_retval(self, arg):
        if '__return__' in self.curframe.f_locals:
            self.displayhook(self.curframe.f_locals['__return__'])
        else:
            self.error('not yet returned')
    do_rv = do_retval

    def do_alias(self, arg):
        args = arg.split(None, 1)
        if not args:
            for k, v in self.aliases.items():
                self.message('{} = {}'.format(k, v))
            return
        if len(args) == 1:
            v = self.aliases.get(args[0])
            self.message('{} = {}'.format(args[0], v) if v else
                            'undefined alias {!r}'.format(args[0]))
            return
        self.aliases[args[0]] = args[1]

    def do_unalias(self, arg):
        self.aliases.pop(arg, None)


HELP_TEXT = """\
Documented commands (type help <topic>):
========================================
EOF    bt         disable    h        list      p     r        u
a      c          do         help     ll        pp    return   unalias
alias  cl         down       j        n         q     s        until
args   clear      enable     jump     next      quit  step     up
b      condition  exit       l        p         retval w        where
break  continue   ignore     debug
"""


# ---- module-level convenience ---------------------------------------------

def set_trace(*, header=None):
    pdb = Pdb()
    if header is not None:
        pdb.message(header)
    pdb.set_trace(sys._getframe().f_back)


def post_mortem(t=None):
    if t is None:
        t = sys.exc_info()[2]
    if t is None:
        raise ValueError('no traceback found, nothing to debug')
    p = Pdb()
    p.reset()
    p.interaction(None, t)


def pm():
    post_mortem(getattr(sys, 'last_traceback', None))


def run(statement, globals=None, locals=None):
    Pdb().run(statement, globals, locals)


def runeval(expression, globals=None, locals=None):
    return Pdb().runeval(expression, globals, locals)


def runctx(statement, globals, locals):
    run(statement, globals, locals)


def runcall(*args, **kwargs):
    func = args[0]
    return Pdb().runcall(func, *args[1:], **kwargs)


def main():
    """``python -m pdb script.py``."""
    import argparse
    parser = argparse.ArgumentParser(prog='pdb')
    parser.add_argument('-c', dest='commands', action='append', default=[])
    parser.add_argument('script', nargs='?')
    parser.add_argument('args', nargs='*')
    opts, _ = parser.parse_known_args()
    if not opts.script:
        parser.print_help()
        sys.exit(1)
    sys.argv[1:] = [opts.script] + list(opts.args)
    sys.path.insert(0, os.path.dirname(os.path.abspath(opts.script)))
    pdb = Pdb()
    pdb.rcLines.extend(opts.commands)
    while True:
        try:
            with open(opts.script) as f:
                code = compile(f.read(), opts.script, 'exec')
            pdb._wait_for_mainpyfile = True
            pdb.mainpyfile = pdb.canonic(opts.script)
            pdb.run(code,
                     globals={'__name__': '__main__', '__file__': opts.script})
            break
        except Restart:
            print('Restarting', opts.script, 'with arguments:', opts.args)
        except SystemExit:
            break
        except Exception as exc:
            traceback_module = __import__('traceback')
            traceback_module.print_exc()
            t = sys.exc_info()[2]
            pdb.interaction(None, t)
            break


if __name__ == '__main__':
    main()
