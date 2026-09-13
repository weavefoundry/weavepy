"""Measure immutable constant loads and tuple lengths in warmed native code."""

from pathlib import Path
import runpy

HERE = Path(__file__).resolve().parent
KERNELS = {
    "tuple_literal_lengths": '''
def calculate(n):
    seq = (1, 2, 3)
    total = 0
    for i in range(n):
        total += len(seq)
    return total
def bench(n):
    total = 0
    for _ in range(n):
        total += calculate(2048)
    assert total == n * 6144
    return total
''',
    "tuple_parameter_lengths": '''
DATA = tuple(range(64))
def calculate(data, n):
    total = 0
    for i in range(n):
        total += len(data)
    return total
def bench(n):
    total = 0
    for _ in range(n):
        total += calculate(DATA, 2048)
    assert total == n * 131072
    return total
''',
    "tuple_constant_returns": '''
def calculate(n):
    value = (None, "shared literal", (1, 2, 3))
    for i in range(n):
        value = (None, "shared literal", (1, 2, 3))
    return value
def bench(n):
    for _ in range(n):
        result = calculate(2048)
    assert result == (None, "shared literal", (1, 2, 3))
    return result
''',
    "string_constant_returns": '''
def calculate(n):
    value = "shared string with spaces"
    for i in range(n):
        value = "shared string with spaces"
    return value
def bench(n):
    for _ in range(n):
        result = calculate(2048)
    assert result == "shared string with spaces"
    return result
''',
    "short_string_activations": '''
def calculate(n):
    value = "shared string with spaces"
    for i in range(n):
        value = "shared string with spaces"
    return value
def bench(n):
    for _ in range(n * 100):
        result = calculate(4)
    assert result == "shared string with spaces"
    return result
''',
}


if __name__ == "__main__":
    runpy.run_path(str(HERE / "jit_probes.py"))["main"](KERNELS)
