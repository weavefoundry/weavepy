"""Compare lazy-cache regression behavior with CPython and the preceding release."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import time

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--base', required=True, type=Path)
parser.add_argument('--new', required=True, type=Path)
parser.add_argument('--out', required=True, type=Path)
parser.add_argument('--test', type=Path, default=Path('tests/regrtest/test_lazy_code_caches.py'))
args = parser.parse_args()
report = {'binaries': {}, 'source': {'path': str(args.test),
          'sha256': hashlib.sha256(args.test.read_bytes()).hexdigest()}, 'runs': {}}
reference = subprocess.run(['python3.14', str(args.test)], text=True, capture_output=True, timeout=120)
report['runs']['cpython'] = {'returncode': reference.returncode, 'stdout': reference.stdout,
                             'stderr': reference.stderr}
passed = reference.returncode == 0
for name, binary in [('base', args.base), ('new', args.new)]:
    report['binaries'][name] = {'path': str(binary), 'sha256': hashlib.sha256(binary.read_bytes()).hexdigest()}
    for mode, flags, setting in [('jit', [], '1'), ('interp', [], '0'), ('gil0', ['-X', 'gil=0'], '1')]:
        start = time.monotonic()
        run = subprocess.run([str(binary), *flags, str(args.test)],
                             env={**os.environ, 'WEAVEPY_JIT': setting},
                             text=True, capture_output=True, timeout=120)
        ok = run.returncode == 0 and run.stdout == reference.stdout
        report['runs'][name + '_' + mode] = {'returncode': run.returncode,
            'stdout': run.stdout, 'stderr': run.stderr, 'passed': ok,
            'seconds': time.monotonic() - start}
        passed &= ok
        print(name, mode, 'PASS' if ok else 'FAIL', flush=True)
args.out.parent.mkdir(parents=True, exist_ok=True)
args.out.write_text(json.dumps(report, indent=2) + '\n')
raise SystemExit(not passed)
