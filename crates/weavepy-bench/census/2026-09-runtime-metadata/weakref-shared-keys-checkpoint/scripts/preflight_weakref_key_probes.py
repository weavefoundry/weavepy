"""Verify all weakref performance fixtures before timed measurement."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess

root = Path('target/weakref-shared-keys-preflight')
assert not root.exists()
root.mkdir()
inputs = Path('target/weakref-shared-keys-inputs')
shutil.copytree(inputs, root / 'inputs')
base = str(Path('target/release/weavepy-runtime-gc-borrowed-handles').resolve())
new = str(Path('target/release/weavepy-runtime-weakref-shared-keys').resolve())
variants = [('cpython', shutil.which('python3.14'), [], None),
            ('previous_jit', base, [], '1'), ('previous_interp', base, [], '0'),
            ('jit', new, [], '1'), ('interp', new, [], '0'), ('gil0', new, ['-X', 'gil=0'], '0')]
report = {'purpose': __doc__, 'binaries': {}, 'rows': {}}
for label, binary, _, _ in variants:
    report['binaries'][label] = {'path': binary, 'sha256': hashlib.sha256(Path(binary).read_bytes()).hexdigest()}
for group in ['heaps', 'access', 'construction']:
    prepared = json.loads((inputs / group / 'preparation.json').read_text())
    for name, case in prepared['rows'].items():
        for key, hashkey in [('path', 'source_sha256'), ('driver', 'driver_sha256')]:
            assert hashlib.sha256(Path(case[key]).read_bytes()).hexdigest() == case[hashkey]
        row = report['rows'][group + '/' + name] = {'case': case, 'checks': {}}
        for label, binary, flags, jit in variants:
            env = dict(os.environ)
            for key in ['WEAVEPY_JIT', 'WEAVEPY_GIL', 'WEAVEPY_VM_STATS', 'WEAVEPY_JIT_TRACE', 'WEAVEPY_JIT_THRESHOLD']:
                env.pop(key, None)
            if jit is not None:
                env['WEAVEPY_JIT'] = jit
                env['WEAVEPY_FROZEN_CACHE'] = str((root / 'frozen' / report['binaries'][label]['sha256']).resolve())
                env['WEAVEPY_STDLIB_CACHE'] = str(Path('target/performance-stdlib-cache').resolve())
            command = [binary, *flags, case['driver']]
            result = subprocess.run(command, env=env, capture_output=True, text=True, timeout=180)
            row['checks'][label] = {'command': command, 'returncode': result.returncode, 'stdout': result.stdout, 'stderr': result.stderr}
            (root / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
            assert result.returncode == 0, (group, name, label, result.stderr)
            values = [json.loads(line) for line in result.stdout.splitlines()]
            assert len(values) == 2 and values[0] == values[1], (name, label, values)
            assert result.stdout == row['checks']['cpython']['stdout'], (name, label, row['checks'])
        print(group, name, 'passed all six modes', flush=True)
assert len(report['rows']) == 19
report['status'] = 'passed'
(root / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
