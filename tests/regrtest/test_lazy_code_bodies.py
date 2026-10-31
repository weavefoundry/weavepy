"""Code objects of cached stdlib modules, whose bodies decode on first read.

Everything a code object exposes has to look the same whether or not its
function ever ran, including in a forked child and through the paths that
copy a code object.
"""

import contextlib
import copy
import dis
import marshal
import os
import sys
import textwrap
import types


def never_called_functions():
    # Functions of modules loaded from the code cache that this program never
    # calls before reading their code.
    candidates = [
        textwrap.shorten,
        textwrap.TextWrapper._wrap_chunks,
        contextlib.contextmanager,
        copy._deepcopy_dict,
        types.prepare_class,
        os.walk,
    ]
    # (Some of these may be native in an implementation.)
    return [f for f in candidates if hasattr(f, "__code__")]


def check(function):
    code = function.__code__
    # Header fields.
    assert code.co_name == function.__name__
    assert code.co_qualname == function.__qualname__
    assert code.co_filename.endswith((".py", ">")), code.co_filename
    assert code.co_argcount >= 0
    # Body fields.
    assert len(code.co_code) > 0
    assert code.co_nlocals == len(code.co_varnames)
    assert isinstance(code.co_consts, tuple)
    assert isinstance(code.co_names, tuple)
    assert code.co_firstlineno > 0
    assert list(code.co_lines())
    assert list(code.co_positions())
    assert any(True for _ in dis.get_instructions(code))
    # Copies and comparisons.
    assert code == code
    assert hash(code) == hash(code)
    replaced = code.replace(co_name="other")
    assert replaced.co_name == "other"
    assert replaced.co_code == code.co_code
    assert replaced.co_consts == code.co_consts
    assert code.replace() == code
    round_tripped = marshal.loads(marshal.dumps(code))
    assert round_tripped == code
    assert round_tripped.co_consts == code.co_consts


def nested_code_objects(code):
    for constant in code.co_consts:
        if isinstance(constant, types.CodeType):
            yield constant
            yield from nested_code_objects(constant)


def check_module_code():
    # A nested code object reached through its parent's constants.
    function = textwrap.TextWrapper.__init__
    assert function.__code__.co_varnames[0] == "self"
    nested = list(nested_code_objects(contextlib.contextmanager.__code__))
    assert nested[0].co_name == "helper"


def check_calls():
    # Running a function whose body wasn't read yet.
    assert textwrap.dedent("  a\n  b") == "a\nb"
    assert textwrap.fill("a b", width=1) == "a\nb"


def check_in_forked_child():
    if not hasattr(os, "fork"):
        return
    pid = os.fork()
    if pid == 0:
        status = 1
        try:
            # (Bodies this process hasn't read yet.)
            import shlex

            assert shlex.quote("a b") == "'a b'"
            assert shlex.split.__code__.co_varnames[0] == "s"
            status = 0
        finally:
            os._exit(status)
    _, status = os.waitpid(pid, 0)
    assert os.waitstatus_to_exitcode(status) == 0


for function in never_called_functions():
    check(function)
check_module_code()
check_calls()
check_in_forked_child()
assert "shlex" not in sys.modules or sys.modules["shlex"].split.__code__.co_name == "split"
print("ok")
