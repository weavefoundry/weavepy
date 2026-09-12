"""Verify cache invalidation and actual native execution across class changes."""
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
    parser.add_argument('--base', required=True)
    parser.add_argument('--new', required=True)
    parser.add_argument('--python', default='python3.14')
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    report = {'binaries': {
        name: {'path': path, 'sha256': hashlib.sha256(Path(path).read_bytes()).hexdigest()}
        for name, path in [('base', args.base), ('new', args.new)]}, 'runs': {}}
    for name, binary, flags, jit in [
        ('cpython', args.python, [], '0'), ('base', args.base, [], '1'),
        ('new', args.new, [], '1'), ('new_interp', args.new, [], '0'),
        ('new_gil0', args.new, ['-X', 'gil=0'], '1'),
    ]:
        env = {**os.environ, 'WEAVEPY_JIT': jit, 'WEAVEPY_JIT_THRESHOLD': '3',
               'WEAVEPY_JIT_TRACE': '1',
               'WEAVEPY_STDLIB_CACHE': str(ROOT / 'target/performance-stdlib-cache')}
        run = subprocess.run([binary, *flags, str(ROOT / 'tests/regrtest/test_class_version_tokens.py')],
                             env=env, text=True, capture_output=True, timeout=180)
        compiled = sorted(set(re.findall(r'jit compile "([^"]+)"', run.stderr)))
        report['runs'][name] = {'returncode': run.returncode, 'stdout': run.stdout,
                                'stderr': run.stderr, 'compiled': compiled}
        print(name, run.returncode, 'native kernels:',
              sorted(set(compiled) & {'native_total', 'write_total', 'slot_total'}), flush=True)
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(report, indent=2) + '\n')
    assert all(row['returncode'] == 0 and row['stdout'] == 'ok\n' for row in report['runs'].values())
    assert {'native_total', 'write_total', 'slot_total'} <= set(report['runs']['new']['compiled'])


if __name__ == '__main__':
    main()
