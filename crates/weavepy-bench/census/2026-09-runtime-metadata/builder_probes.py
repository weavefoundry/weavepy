"""Measure checked list builders with the paired JIT timing and RSS harness."""

from pathlib import Path
import runpy

HERE = Path(__file__).resolve().parent
KERNELS = {}
for name, value, expected in (
    ("list_int_builder", "i * i", "[i * i for i in range(1024)]"),
    ("list_float_builder", "i + 0.5", "[i + 0.5 for i in range(1024)]"),
    ("list_bool_builder", "i < 512", "[True] * 512 + [False] * 512"),
):
    KERNELS[name] = f'''
EXPECTED = {expected}
def build(size):
    out = []
    for i in range(size):
        out.append({value})
    return out
def bench(n):
    for _ in range(n):
        result = build(1024)
    assert result == EXPECTED
    assert type(result[0]) is type(EXPECTED[0])
    assert type(result[-1]) is type(EXPECTED[-1])
    return len(result)
'''

KERNELS["byte_list_builder"] = '''
DATA = bytes(range(256)) * 4
KEY = bytes(range(16))
EXPECTED = bytes(value ^ (index % 16) for index, value in enumerate(DATA))
def build(data, key):
    out = []
    klen = len(key)
    for index, value in enumerate(data):
        out.append((value ^ key[index % klen]) & 255)
    return bytes(out)
def bench(n):
    for _ in range(n):
        result = build(DATA, KEY)
    assert result == EXPECTED
    return len(result)
'''


if __name__ == "__main__":
    runpy.run_path(str(HERE / "jit_probes.py"))["main"](KERNELS)
