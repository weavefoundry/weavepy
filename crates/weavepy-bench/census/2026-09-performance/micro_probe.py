# Exact supplemental measurement workload and ordering.
# Run from the repository root after saving the baseline binary as
# target/release/weavepy-perf-base. Results go to tmp/performance-20260908.
"""Supplemental call, source-compilation, and cached-import measurements."""
import json, os, statistics, subprocess, time
from pathlib import Path

Path("tmp/performance-20260908").mkdir(parents=True, exist_ok=True)

binaries={'base':'target/release/weavepy-perf-base','new':'target/release/weavepy','cpython':'python3.14'}
workloads={
'calls': '''
def f(x):
    return x

def bench():
    total = 0
    for i in range(500000):
        total += f(i)
    return total
''',
'compile': '''
source = """class Point:
    def __init__(self, x, y):
        self.x, self.y = x, y
    def length2(self):
        return self.x * self.x + self.y * self.y

def process(n):
    return [Point(i, i + 1).length2() for i in range(n) if i % 2]
"""
def bench():
    for _ in range(1500):
        compile(source, "<compile-bench>", "exec")
''',
'cached_import': '''
import math

def bench():
    for _ in range(100000):
        import math
'''
}
report={}
for name, source in workloads.items():
    code=source+'\nimport time\nstart = time.perf_counter_ns()\nbench()\nprint(time.perf_counter_ns() - start)\n'
    values={label:[] for label in binaries}
    for run in range(8):
        for label in list(binaries) if run%2==0 else list(reversed(binaries)):
            env={**os.environ,'WEAVEPY_JIT':'0'}
            result=subprocess.check_output([binaries[label],'-c',code],env=env,text=True)
            if run: values[label].append(int(result.strip()))
    report[name]=values
    print(name,{label:round(statistics.median(samples)/1e6,2) for label,samples in values.items()},flush=True)
Path('tmp/performance-20260908/micro.json').write_text(json.dumps(report,indent=2)+'\n')
