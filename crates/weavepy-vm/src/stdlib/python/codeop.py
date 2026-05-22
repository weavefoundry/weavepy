"""WeavePy `codeop` — minimal stub for `compile_command`.

Used by `code.InteractiveInterpreter` and REPLs to decide whether a
partial source string can be compiled as a complete statement. This
implementation is conservative: any source that doesn't end in an
obvious continuation token is fed straight to `compile()`.
"""


__all__ = ["compile_command", "Compile", "CommandCompiler"]


def compile_command(source, filename="<input>", symbol="single"):
    if source.endswith("\\"):
        return None
    try:
        return compile(source, filename, symbol)
    except SyntaxError:
        # Heuristic: if it ends with `:` we want more input.
        stripped = source.rstrip()
        if stripped.endswith(":"):
            return None
        # Same for unbalanced parens/brackets/braces.
        opens = sum(source.count(c) for c in "([{")
        closes = sum(source.count(c) for c in ")]}")
        if opens > closes:
            return None
        raise


class Compile:
    def __init__(self):
        self.flags = 0

    def __call__(self, source, filename, symbol):
        return compile(source, filename, symbol)


class CommandCompiler:
    def __init__(self):
        self.compiler = Compile()

    def __call__(self, source, filename="<input>", symbol="single"):
        return compile_command(source, filename, symbol)
