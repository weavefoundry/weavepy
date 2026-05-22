"""WeavePy `code` — `InteractiveInterpreter` / `InteractiveConsole`.

Provides the classic REPL primitives used by `runpy`-driven REPL
shells and tools like `code.interact()`.
"""

import sys
import traceback
import codeop


__all__ = ["InteractiveInterpreter", "InteractiveConsole", "interact", "compile_command"]


compile_command = codeop.compile_command


class InteractiveInterpreter:
    def __init__(self, locals=None):
        if locals is None:
            locals = {"__name__": "__console__", "__doc__": None}
        self.locals = locals
        self.compile = codeop.CommandCompiler()

    def runsource(self, source, filename="<input>", symbol="single"):
        try:
            code = self.compile(source, filename, symbol)
        except (OverflowError, SyntaxError, ValueError):
            self.showsyntaxerror(filename)
            return False
        if code is None:
            return True
        self.runcode(code)
        return False

    def runcode(self, code):
        try:
            exec(code, self.locals)
        except SystemExit:
            raise
        except BaseException:
            self.showtraceback()

    def showsyntaxerror(self, filename=None):
        typ, value, tb = sys.exc_info()
        sys.stderr.write("".join(traceback.format_exception_only(typ, value)))

    def showtraceback(self):
        typ, value, tb = sys.exc_info()
        sys.stderr.write("".join(traceback.format_exception(typ, value, tb)))

    def write(self, data):
        sys.stderr.write(data)


class InteractiveConsole(InteractiveInterpreter):
    def __init__(self, locals=None, filename="<console>"):
        super().__init__(locals)
        self.filename = filename
        self.resetbuffer()

    def resetbuffer(self):
        self.buffer = []

    def push(self, line):
        self.buffer.append(line)
        source = "\n".join(self.buffer)
        more = self.runsource(source, self.filename)
        if not more:
            self.resetbuffer()
        return more

    def raw_input(self, prompt=""):
        return input(prompt)

    def interact(self, banner=None, exitmsg=None):
        if banner is not None:
            self.write(banner + "\n")
        more = False
        while True:
            try:
                prompt = ">>> " if not more else "... "
                line = self.raw_input(prompt)
            except EOFError:
                self.write("\n")
                break
            except KeyboardInterrupt:
                self.write("\nKeyboardInterrupt\n")
                self.resetbuffer()
                more = False
                continue
            more = self.push(line)
        if exitmsg is not None:
            self.write(exitmsg + "\n")


def interact(banner=None, readfunc=None, local=None, exitmsg=None):
    console = InteractiveConsole(local)
    if readfunc is not None:
        console.raw_input = readfunc
    console.interact(banner, exitmsg)
