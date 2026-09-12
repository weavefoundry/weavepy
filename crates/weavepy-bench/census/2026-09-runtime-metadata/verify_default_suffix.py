"""Check trailing defaults and prove both native frame paths execute."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess

ROOT = Path(__file__).resolve().parents[4]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True)
    parser.add_argument('--out', required=True, type=Path)
    args = parser.parse_args()
    env = {**os.environ, 'WEAVEPY_STDLIB_CACHE': str(ROOT / 'target/performance-stdlib-cache'),
           'WEAVEPY_JIT_THRESHOLD': '3', 'WEAVEPY_JIT_TRACE': '1', 'WEAVEPY_VM_STATS': '1'}
    test = ROOT / 'tests/regrtest/test_default_suffix.py'
    runs = {}
    for name, binary, flags, jit in (
        ('cpython', 'python3.14', [], '0'), ('jit', args.binary, [], '1'),
        ('interp', args.binary, [], '0'), ('gil0', args.binary, ['-X', 'gil=0'], '1'),
    ):
        proc = subprocess.run([binary, *flags, str(test)], env={**env, 'WEAVEPY_JIT': jit},
                              text=True, capture_output=True, timeout=180)
        runs[name] = {'returncode': proc.returncode, 'stdout': proc.stdout, 'stderr': proc.stderr}
        print(name, proc.returncode, flush=True)
    proofs = {}
    for name, expression, leaf in (('scalar', 'value + 1', True), ('context', 'value + BIAS', False)):
        source = f'''
BIAS = 1
def original(a=10, b=20, c=30):
    return a + b + c
def suffix_target(value):
    return {expression}
original.__code__ = suffix_target.__code__
def suffix_total(n):
    total = 0
    for _ in range(n):
        total += original()
    return total
for _ in range(60):
    assert suffix_total(50) == 1550
assert suffix_total(10000) == 310000
'''
        proc = subprocess.run([args.binary, '-c', source], env={**env, 'WEAVEPY_JIT': '1'},
                              text=True, capture_output=True, timeout=180)
        count = re.search(r'native-to-native calls: \*\*(\d+)\*\*', proc.stderr)
        scalar = re.search(r'scalar leaf calls: \*\*(\d+)\*\*', proc.stderr)
        proofs[name] = {'source': source, 'returncode': proc.returncode,
                        'stdout': proc.stdout, 'stderr': proc.stderr,
                        'native_calls': int(count[1]) if count else 0,
                        'scalar_calls': int(scalar[1]) if scalar else 0,
                        'expected_leaf': leaf,
                        'compiled': bool(re.search(
                            r'jit compile "suffix_target" \([^\n]*scalar leaf ' + str(leaf).lower(),
                            proc.stderr))}
        print(name, 'native calls', proofs[name]['native_calls'],
              'scalar calls', proofs[name]['scalar_calls'], flush=True)
    report = {'runs': runs, 'proofs': proofs, 'binary': {
        'path': args.binary, 'sha256': hashlib.sha256(Path(args.binary).read_bytes()).hexdigest()},
        'test': {'path': str(test.relative_to(ROOT)), 'source': test.read_text(),
                 'sha256': hashlib.sha256(test.read_bytes()).hexdigest()}}
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(report, indent=2) + '\n')
    assert all(row['returncode'] == 0 and row['stdout'] == 'default suffix semantics: ok\n'
               for row in runs.values())
    for proof in proofs.values():
        assert proof['returncode'] == 0 and proof['compiled'] and proof['native_calls'] >= 10000
        if proof['expected_leaf']:
            assert proof['scalar_calls'] >= 10000
        else:
            assert proof['native_calls'] - proof['scalar_calls'] >= 10000


if __name__ == '__main__':
    main()
