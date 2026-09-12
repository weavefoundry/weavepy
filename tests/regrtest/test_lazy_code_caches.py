"""Exercise first cache writes, independent code copies, and observer transitions."""
import dis
import marshal
import sys
import threading
import types


SOURCE = "def calculate(value):\n    result = value + offset\n    return result\n"


def make_function(offset):
    namespace = {"offset": offset}
    exec(compile(SOURCE, "lazy_cache_case.py", "exec"), namespace)
    return namespace["calculate"]


def observed_lines(function):
    lines = []

    def trace(frame, event, arg):
        if frame.f_code is function.__code__ and event == "line":
            lines.append(frame.f_lineno)
        return trace

    sys.settrace(trace)
    try:
        assert function(5) == 12
    finally:
        sys.settrace(None)
    return lines


function = make_function(7)
wire = function.__code__.co_code
encoded = marshal.dumps(function.__code__)
instructions = [(item.opname, item.arg) for item in dis.get_instructions(function)]
cold_lines = observed_lines(function)
assert cold_lines == [2, 3]
for i in range(1000):
    assert function(i) == i + 7
assert function.__code__.co_code == wire
assert marshal.dumps(function.__code__) == encoded
assert [(item.opname, item.arg) for item in dis.get_instructions(function)] == instructions
assert observed_lines(function) == cold_lines

for code in (function.__code__.replace(), marshal.loads(encoded)):
    independent = types.FunctionType(code, {"offset": 1000})
    for i in range(500):
        assert independent(i) == i + 1000
        assert function(i) == i + 7

replacement = make_function(2000)
replacement.__code__ = function.__code__.replace()
assert replacement(3) == 2003
function.__globals__["offset"] = 19
assert function(3) == 22
assert replacement(3) == 2003

functions = [make_function(31 * i) for i in range(4)]
barrier = threading.Barrier(4)
results = [None] * 4
errors = []


def worker(index):
    try:
        current = functions[index]
        barrier.wait()
        total = 0
        for i in range(1000):
            total += current(i)
        results[index] = total
    except BaseException as error:
        errors.append(type(error).__name__)


threads = [threading.Thread(target=worker, args=(i,)) for i in range(4)]
for thread in threads:
    thread.start()
for thread in threads:
    thread.join()
assert not errors, errors
assert results == [499500 + 31000 * i for i in range(4)], results
print("lazy code cache semantics: ok")
