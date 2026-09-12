"""Measure verified datetime components with separate stable per-binary frozen caches."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import runpy
import shutil
import subprocess

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--binary', required=True)
p.add_argument('--previous')
p.add_argument('--inputs', type=Path, required=True)
p.add_argument('--out', type=Path, required=True)
p.add_argument('--samples', type=int, default=7)
a = p.parse_args()
assert a.samples > 0 and not a.out.exists()
a.out.mkdir(parents=True)
shutil.copytree(a.inputs, a.out / 'inputs')
prepared = json.loads((a.inputs / 'preparation.json').read_text())
bench = runpy.run_path('tools/bench_compare.py')
context = bench['execution_context']()
assert context['declared_launch_context'] == 'outside-tool-filesystem-sandbox (require_escalated)'
variants = [('jit', a.binary, '1'), ('interp', a.binary, '0'),
            ('cpython', shutil.which('python3.14'), None)]
if a.previous:
    variants[:0] = [('previous_jit', a.previous, '1'), ('previous_interp', a.previous, '0')]
report = {'purpose': __doc__, 'execution_context': context, 'samples': a.samples,
          'work': prepared['work'], 'binaries': {}, 'rows': {}, 'verification': {}}
for label, binary, _ in variants:
    path = Path(binary).resolve()
    report['binaries'][label] = {'path': str(path), 'sha256': hashlib.sha256(path.read_bytes()).hexdigest()}

cache_root = a.out / 'frozen'
cache_root.mkdir()
cache_for = {label: cache_root / info['sha256']
             for label, info in report['binaries'].items() if label != 'cpython'}
report['frozen_caches'] = {label: str(path.resolve()) for label, path in cache_for.items()}
report['measurement_inputs'] = {name: hashlib.sha256(Path(name).read_bytes()).hexdigest()
                               for name in ['tools/bench_compare.py', __file__]}

def save():
    (a.out / 'measurements.json').write_text(json.dumps(report, indent=2) + '\n')

for name, case in prepared['rows'].items():
    for key in ['path', 'driver']:
        source = Path(case[key])
        expected = case['source_sha256' if key == 'path' else 'driver_sha256']
        assert hashlib.sha256(source.read_bytes()).hexdigest() == expected
    results = report['verification'][name] = {}
    for label, binary, jit in [*variants, ('gil0', a.binary, '1')]:
        env = dict(os.environ)
        for key in ['WEAVEPY_JIT', 'WEAVEPY_GIL', 'WEAVEPY_VM_STATS', 'WEAVEPY_JIT_TRACE', 'WEAVEPY_JIT_THRESHOLD']:
            env.pop(key, None)
        if jit is not None:
            env['WEAVEPY_JIT'] = jit
            env['WEAVEPY_FROZEN_CACHE'] = str(cache_for.get(label, cache_for['jit']).resolve())
        flags = ['-X', 'gil=0'] if label == 'gil0' else []
        result = subprocess.run([binary, *flags, case['driver']], env=env, capture_output=True, text=True, timeout=120)
        results[label] = {'returncode': result.returncode, 'stdout': result.stdout, 'stderr': result.stderr}
        save()
        assert result.returncode == 0, (name, label, result.stderr)
        values = [json.loads(line) for line in result.stdout.splitlines()]
        assert len(values) == 2 and values[0] == values[1], (name, label, values)
    expected = results['cpython']['stdout']
    assert all(result['stdout'] == expected for result in results.values()), (name, results)
    print(name, 'values verified before and after warmup', flush=True)

for name, case in prepared['rows'].items():
    row = {'case': case, 'samples': {label: [] for label, _, _ in variants}}
    report['rows'][name] = row
    for cycle in range(a.samples + 1):
        for label, binary, jit in variants if cycle % 2 == 0 else reversed(variants):
            sample = bench['measure'](binary, jit, name, prepared['work'], True, fixture_root=a.inputs, frozen_cache=cache_for.get(label))
            if cycle:
                row['samples'][label].append(sample)
            save()
        if cycle == 0:
            frozen_before = bench['frozen_cache_snapshot'](cache_root)
    frozen_after = bench['frozen_cache_snapshot'](cache_root)
    row['frozen_cache'] = {'before': frozen_before, 'after': frozen_after,
                           'unchanged': frozen_before == frozen_after}
    save()
    assert frozen_before == frozen_after, ('Frozen artifacts changed during timing', name)
    row['cpython_comparisons'] = {mode: bench['relative_metrics'](row['samples'][mode], row['samples']['cpython'])
                                 for mode in ['jit', 'interp']}
    if a.previous:
        row['previous_comparisons'] = {mode: bench['relative_metrics'](row['samples'][mode], row['samples']['previous_' + mode])
                                      for mode in ['jit', 'interp']}
    save()
    print(name, row.get('previous_comparisons'), row['cpython_comparisons'], flush=True)
