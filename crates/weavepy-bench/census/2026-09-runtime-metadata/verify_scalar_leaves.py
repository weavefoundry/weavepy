"""Prove scalar leaf entry and compare binding and fallback behavior."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[3]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--base', required=True)
    parser.add_argument('--new', required=True)
    parser.add_argument('--out', required=True, type=Path)
    args = parser.parse_args()
    env = {**os.environ, 'WEAVEPY_STDLIB_CACHE': str(ROOT / 'target/performance-stdlib-cache'),
           'WEAVEPY_JIT_THRESHOLD': '3', 'WEAVEPY_JIT_TRACE': '1', 'WEAVEPY_VM_STATS': '1'}
    runs = {}
    for name, binary, flags, jit in (
        ('cpython', 'python3.14', [], '0'), ('base', args.base, [], '1'),
        ('new', args.new, [], '1'), ('new_interp', args.new, [], '0'),
        ('new_gil0', args.new, ['-X', 'gil=0'], '1'),
    ):
        proc = subprocess.run([binary, *flags, str(ROOT / 'tests/regrtest/test_jit_scalar_leaf_calls.py')],
                              env={**env, 'WEAVEPY_JIT': jit}, text=True, capture_output=True, timeout=180)
        runs[name] = {'returncode': proc.returncode, 'stdout': proc.stdout, 'stderr': proc.stderr}
        print(name, proc.returncode, flush=True)
    source = '''
def scalar_leaf(a, b=1):
    return a + b

def scalar_loop(n):
    total = 0
    for i in range(n):
        total += scalar_leaf(i)
    return total

for _ in range(60):
    assert scalar_loop(50) == 1275
assert scalar_loop(102400) == 5242931200
'''
    proc = subprocess.run([args.new, '-c', source], env={**env, 'WEAVEPY_JIT': '1'},
                          text=True, capture_output=True, timeout=180)
    count = re.search(r'scalar leaf calls: \*\*(\d+)\*\*', proc.stderr)
    proof = {'returncode': proc.returncode, 'stdout': proc.stdout, 'stderr': proc.stderr,
             'scalar_leaf_calls': int(count[1]) if count else 0,
             'leaf_compiled': bool(re.search(r'jit compile "scalar_leaf" \([^\n]*scalar leaf true', proc.stderr)),
             'source': source}
    print('native scalar leaf entries:', proof['scalar_leaf_calls'], flush=True)
    report = {'runs': runs, 'entry_proof': proof, 'binaries': {
        name: {'path': binary, 'sha256': hashlib.sha256(Path(binary).read_bytes()).hexdigest()}
        for name, binary in (('base', args.base), ('new', args.new))}}
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(report, indent=2) + '\n')
    assert all(row['returncode'] == 0 and row['stdout'] == 'ok\n' for row in runs.values())
    assert proof['returncode'] == 0 and proof['leaf_compiled'] and proof['scalar_leaf_calls'] >= 102400


if __name__ == '__main__':
    main()
