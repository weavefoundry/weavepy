"""Check native slice bounds and producer-specific fallback against CPython."""
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
    test = ROOT / 'tests/regrtest/test_native_slice_bounds.py'
    env = {**os.environ, 'WEAVEPY_STDLIB_CACHE': str(ROOT / 'target/performance-stdlib-cache'),
           'WEAVEPY_JIT_THRESHOLD': '3', 'WEAVEPY_JIT_TRACE': '1', 'WEAVEPY_VM_STATS': '1'}
    runs = {}
    for name, binary, flags, jit in [
        ('cpython', 'python3.14', [], '0'), ('jit', args.binary, [], '1'),
        ('interp', args.binary, [], '0'), ('gil0', args.binary, ['-X', 'gil=0'], '1'),
    ]:
        run = subprocess.run([binary, *flags, str(test)], env={**env, 'WEAVEPY_JIT': jit},
                             capture_output=True, text=True, timeout=180)
        runs[name] = {'returncode': run.returncode, 'stdout': run.stdout, 'stderr': run.stderr,
                      'compiled': sorted(set(re.findall(r'jit compile "([^"]+)"', run.stderr)))}
        print(name, run.returncode, flush=True)
    stats = {}
    for label in ['native entries', 'direct native calls', 'native-to-native calls']:
        match = re.search(re.escape(label) + r': \*\*(\d+)\*\*', runs['jit']['stderr'])
        stats[label] = int(match[1]) if match else 0
    report = {'binary': {'path': args.binary, 'sha256': hashlib.sha256(Path(args.binary).read_bytes()).hexdigest()},
              'test': {'path': str(test.relative_to(ROOT)), 'sha256': hashlib.sha256(test.read_bytes()).hexdigest()},
              'runs': runs, 'native_counters': stats}
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(report, indent=2) + '\n')
    assert all(row['returncode'] == 0 and row['stdout'] == 'native slice bounds: ok\n' for row in runs.values())
    targets = {'list_stop', 'str_stop', 'list_pair', 'str_pair', 'list_open', 'str_open', 'two_bounds'}
    assert targets <= set(runs['jit']['compiled']), 'A target slice function did not compile.'
    assert sum(stats.values()) >= 100, 'Native execution was not exercised.'
    assert 'jit deopt "two_bounds"' in runs['jit']['stderr'], 'Two-bound native fallback was not exercised.'
    print('native counters', stats, flush=True)


if __name__ == '__main__':
    main()
