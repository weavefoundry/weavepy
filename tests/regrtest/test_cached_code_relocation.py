"""Relocated cached code keeps nested filenames, behavior, and tracebacks."""

import importlib.util
import os
import py_compile
import subprocess
import sys
import tempfile


SOURCE = '''
def outer(x):
    def inner(y):
        return x + y
    return inner

class Box:
    def value(self):
        return [x * x for x in range(4)]

def values():
    yield from range(3)

async def coro():
    return 9

def fail():
    return 1 / 0
'''

CHECK = '''
import os, sys, types
import relocated_target as m
expected = os.path.join(sys.argv[1], 'relocated_target.py')
def check(code):
    assert code.co_filename == expected, (code.co_filename, expected)
    for value in code.co_consts:
        if isinstance(value, types.CodeType):
            check(value)
for f in (m.outer, m.outer(2), m.Box.value, m.values, m.coro, m.fail):
    check(f.__code__)
assert m.outer(2)(3) == 5
assert m.Box().value() == [0, 1, 4, 9]
assert list(m.values()) == [0, 1, 2]
c = m.coro()
try:
    c.send(None)
except StopIteration as e:
    assert e.value == 9
else:
    raise AssertionError('coroutine did not return')
try:
    m.fail()
except ZeroDivisionError as e:
    tb = e.__traceback__
    while tb.tb_next is not None:
        tb = tb.tb_next
    assert tb.tb_frame.f_code.co_filename == expected
else:
    raise AssertionError('missing exception')
print('relocated code: ok')
'''

with tempfile.TemporaryDirectory() as directory:
    directory = os.path.realpath(directory)
    path = os.path.join(directory, 'relocated_target.py')
    with open(path, 'w') as f:
        f.write(SOURCE)
    cached = py_compile.compile(path, dfile='old/location/relocated_target.py', doraise=True)
    assert cached == importlib.util.cache_from_source(path)
    with open(cached, 'rb') as f:
        original = f.read()
    # The healthy cache stays unchanged, proving relocation happens in memory.
    for _ in range(2):
        result = subprocess.run([sys.executable, '-S', '-c', CHECK, directory],
                                cwd=directory, capture_output=True, text=True, timeout=30)
        assert result.returncode == 0, (result.stdout, result.stderr)
        assert result.stdout == 'relocated code: ok\n', result.stdout
        with open(cached, 'rb') as f:
            assert f.read() == original
    # A truncated cache must still fall back to recompiling the source.
    with open(cached, 'wb') as f:
        f.write(b'bad')
    result = subprocess.run([sys.executable, '-S', '-c', CHECK, directory],
                            cwd=directory, capture_output=True, text=True, timeout=30)
    assert result.returncode == 0, (result.stdout, result.stderr)
    assert result.stdout == 'relocated code: ok\n', result.stdout

print('cached code relocation: ok')
