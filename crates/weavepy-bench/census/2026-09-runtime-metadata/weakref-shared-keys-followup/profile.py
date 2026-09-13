"""Capture live allocation stacks for ordinary, finalizable, and watched heaps.

These instrumented live allocations are not execution times, allocation churn,
or OS peak RSS. Keep all zero-count controls and both release identities.
"""
import argparse
import gzip
import hashlib
import json
import os
from pathlib import Path
import selectors
import subprocess

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--binary', type=Path, required=True)
p.add_argument('--inputs', type=Path, required=True)
p.add_argument('--out', type=Path, required=True)
a = p.parse_args()
assert not a.out.exists()
assert os.environ.get('WEAVEPY_BENCH_LAUNCH_CONTEXT') == 'outside-tool-filesystem-sandbox (require_escalated)'
a.out.mkdir()
binary = a.binary.resolve()
cases = json.loads((a.inputs / 'preparation.json').read_text())['rows']
report = {'purpose': __doc__, 'binary': str(binary),
          'sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
          'instrumentation': 'MallocStackLogging=1; malloc_history -allBySize -fullStacks',
          'launch_context': os.environ['WEAVEPY_BENCH_LAUNCH_CONTEXT'], 'rows': {}}
for name, case in cases.items():
    definitions = Path(case['profile_definitions']).read_text()
    assert hashlib.sha256(definitions.encode()).hexdigest() == case['profile_definitions_sha256']
    for count in [0, 3000]:
        label = f"{case['kind']}_{count}"
        source = definitions + f'''import json, os, time
populate(20)
bench(2)
populate({count})
bench(2)
print(json.dumps([os.getpid(), describe()]), flush=True)
time.sleep(120)
'''
        driver = a.out / (label + '.py')
        driver.write_text(source)
        logs = a.out / (label + '-stacklogs')
        logs.mkdir()
        env = dict(os.environ, WEAVEPY_JIT='0', MallocStackLogging='1',
                   MallocStackLoggingDirectory=str(logs.resolve()),
                   WEAVEPY_FROZEN_CACHE=str((a.out / 'frozen').resolve()),
                   WEAVEPY_STDLIB_CACHE=str(Path('target/performance-stdlib-cache').resolve()))
        env.pop('MallocStackLoggingNoCompact', None)
        child = subprocess.Popen([str(binary), str(driver)], env=env, text=True,
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        row = report['rows'][label] = {'source_sha256': hashlib.sha256(source.encode()).hexdigest(),
                                     'count': count, 'kind': case['kind'], 'jit': False}
        try:
            with selectors.DefaultSelector() as selector:
                selector.register(child.stdout, selectors.EVENT_READ)
                assert selector.select(45), 'Child did not announce readiness.'
            pid, values = json.loads(child.stdout.readline())
            expected = [count, count if case['kind'].startswith('weak_') else 0,
                        0 if count else None, count // 2 if count else None,
                        count - 1 if count else None, True]
            row['values'] = values
            assert pid == child.pid and child.poll() is None and values == expected
            command = ['/usr/bin/malloc_history', str(pid), '-allBySize', '-fullStacks']
            result = subprocess.run(command, capture_output=True, timeout=55)
            (a.out / (label + '.history.txt.gz')).write_bytes(gzip.compress(result.stdout, mtime=0))
            (a.out / (label + '.history-stderr.txt')).write_bytes(result.stderr)
            row.update({'pid': pid, 'command': command, 'returncode': result.returncode,
                        'history_bytes': len(result.stdout), 'history_sha256': hashlib.sha256(result.stdout).hexdigest()})
            assert result.returncode == 0, result.stderr.decode(errors='replace')
            print(label, len(result.stdout), 'bytes of live-allocation history', flush=True)
        finally:
            if child.poll() is None:
                child.terminate()
            try:
                _, errors = child.communicate(timeout=5)
            except subprocess.TimeoutExpired:
                child.kill()
                _, errors = child.communicate(timeout=5)
            (a.out / (label + '.child-stderr.txt')).write_text(errors)
            (a.out / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
