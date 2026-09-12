"""Measure scalar calls in an activation larger than the runtime pin limit."""

from pathlib import Path
import runpy

HERE = Path(__file__).resolve().parent
KERNELS = {}
short = runpy.run_path(str(HERE / "scalar_result_probes.py"))["KERNELS"]
for name, source in short.items():
    argument = "SOURCE" if name == "callable_instance_sum" else "source"
    delta = {"integer_callback_sum": 0, "integer_callback_left": 1,
             "keyword_callback_sum": 2, "callable_instance_sum": 3}[name]
    KERNELS[name] = source.split("def bench(n):")[0] + f'''
def bench(n):
    count = n * 512
    total = collect(count, {argument})
    assert total == count * (count - 1) // 2 + {delta} * count
    return total
'''

if __name__ == "__main__":
    runpy.run_path(str(HERE / "jit_probes.py"))["main"](KERNELS)
