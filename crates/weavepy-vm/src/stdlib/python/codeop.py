r"""WeavePy `codeop` — CPython-shaped `compile_command`.

Used by `code.InteractiveInterpreter` and REPLs to decide whether a
partial source string can be compiled as a complete statement. CPython
detects "incomplete input" with the `PyCF_ALLOW_INCOMPLETE_INPUT`
compiler flag (the parser raises `_IncompleteInputError` when the error
is at end-of-input); WeavePy's parser reports those states with stable
messages, so `_is_incomplete` classifies on them instead.
"""

import warnings

__all__ = ["compile_command", "Compile", "CommandCompiler"]

# CPython compiler flag constants (Include/cpython/compile.h). WeavePy's
# `compile()` tolerates unknown flag bits, so these exist chiefly so
# `from codeop import PyCF_DONT_IMPLY_DEDENT` (test_codeop, IPython)
# resolves and flag arithmetic behaves.
PyCF_DONT_IMPLY_DEDENT = 0x200
PyCF_ALLOW_INCOMPLETE_INPUT = 0x4000

def _is_incomplete(exc, source):
    """Would CPython's tokenizer have raised `_IncompleteInputError`?

    CPython flags an error as *incomplete* only when it happens at
    end-of-input; WeavePy's parser reports these states with stable
    messages, classified here against the original source.
    """
    msg = str(exc)
    # An unclosed bracket is incomplete no matter what follows.
    if "was never closed" in msg:
        return True
    if "unexpected EOF" in msg or "incomplete input" in msg:
        # A backslash-newline at EOF already consumed the continuation:
        # CPython treats "a = 9+ \\\n" as a hard error, while a bare
        # trailing backslash can still be continued.
        if source.endswith("\\\n") or source.endswith("\\\r\n"):
            return False
        return True
    # A pending suite ("if 1:" …) is incomplete only when nothing but
    # blank lines follows the suite *header* — a dedented statement
    # after it ("def x():\n\npass\n") is a real IndentationError. The
    # parser names the header in the message ("… on line N"); its
    # `lineno` is where scanning stopped, which for the incomplete case
    # is the header/EOF and for the invalid case the dedented token.
    if "expected an indented block" in msg:
        import re
        m = re.search(r"on line (\d+)", msg)
        header = int(m.group(1)) if m else (getattr(exc, "lineno", None) or 0)
        rest = source.split("\n")[header:]
        return all(not line.strip() for line in rest)
    # An unterminated string is incomplete when it can continue on the
    # next line: triple-quoted, or single-quoted with the source ending
    # in a line-continuation backslash.
    if "unterminated string literal" in msg or "unterminated triple-quoted" in msg:
        if source.count("'''") % 2 == 1 or source.count('"""') % 2 == 1:
            return True
        return source.endswith("\\")
    # EOF straight after a line-continuation backslash: more input can
    # legitimately follow ("a = 9+ \\"); a backslash-newline at EOF
    # ("a = 9+ \\\n") already consumed the continuation and is invalid.
    if "line continuation character" in msg:
        return source.endswith("\\")
    return False


def _maybe_compile(compiler, source, filename, symbol):
    # Check for source consisting of only blank lines and comments.
    for line in source.split("\n"):
        line = line.strip()
        if line and line[0] != '#':
            break  # Leave it alone.
    else:
        if symbol != "eval":
            source = "pass"  # Replace it with a 'pass' statement.
        else:
            # Blank `eval` input can only become an expression with more
            # text — CPython reports it incomplete (`ai("", "eval")`).
            return None

    # Disable compiler warnings when probing for incomplete input: the
    # winning compile below re-emits them exactly once (CPython behaviour;
    # test_codeop `test_warning` / `test_incomplete_warning`).
    with warnings.catch_warnings():
        warnings.simplefilter("ignore", (SyntaxWarning, DeprecationWarning))
        try:
            compiler(source, filename, symbol)
        except SyntaxError as first:  # Let other compile errors propagate.
            if _is_incomplete(first, source):
                return None
            try:
                compiler(source + "\n", filename, symbol)
                return None
            except SyntaxError as e:
                if _is_incomplete(e, source):
                    return None
                # Fall through: the definitive compile reports the error.
        else:
            # CPython's interactive grammar (PyCF_DONT_IMPLY_DEDENT): a
            # syntactically-complete source that still sits inside an
            # indented suite at EOF — the text after the last newline is
            # non-empty and starts with whitespace — is *incomplete*: the
            # tokenizer refuses to imply the closing DEDENTs until a blank
            # line arrives (`"def x():\n  pass"` vs `"def x():\n  pass\n"`).
            if symbol == "single":
                last_line = source.rpartition("\n")[2]
                if last_line and last_line[0] in " \t":
                    return None
    return compiler(source, filename, symbol)


def compile_command(source, filename="<input>", symbol="single"):
    r"""Compile a command and determine whether it is incomplete.

    Returns a code object if the command is complete and valid, or
    ``None`` if it is incomplete; raises `SyntaxError`/`ValueError`/
    `OverflowError` like `compile()` otherwise.
    """
    return _maybe_compile(_default_compile, source, filename, symbol)


def _default_compile(source, filename, symbol):
    return compile(source, filename, symbol)


class Compile:
    """Instances behave like the built-in `compile`, remembering
    `__future__` flags across calls (CPython parity surface; WeavePy's
    compiler resolves future imports per-unit, so only the flag
    bookkeeping is observable)."""

    def __init__(self):
        self.flags = PyCF_DONT_IMPLY_DEDENT | PyCF_ALLOW_INCOMPLETE_INPUT

    def __call__(self, source, filename, symbol, flags=0):
        self.flags |= flags
        return compile(source, filename, symbol)


class CommandCompiler:
    """Like `compile_command`, but with a stateful `Compile` instance."""

    def __init__(self):
        self.compiler = Compile()

    def __call__(self, source, filename="<input>", symbol="single"):
        return _maybe_compile(self.compiler, source, filename, symbol)
