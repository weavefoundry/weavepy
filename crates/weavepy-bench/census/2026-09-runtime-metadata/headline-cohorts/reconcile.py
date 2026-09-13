"""Reconcile the historical headline with the complete workload census."""
import hashlib
import json
import math
import statistics
import subprocess
from pathlib import Path

out = Path('crates/weavepy-bench/census/2026-09-runtime-metadata/headline-cohorts')
assert not out.exists()
out.mkdir()
revision = subprocess.check_output(['git', 'rev-parse', '14185816'], text=True).strip()
baseline_path = 'crates/weavepy-bench/baselines/bench-macos-aarch64.json'
old_bytes = subprocess.check_output(['git', 'show', revision + ':' + baseline_path])
old = json.loads(old_bytes)
latest_path = Path('crates/weavepy-bench/census/2026-09-runtime-metadata/weakref-shared-keys-followup/suite.json')
latest = json.loads(latest_path.read_text())
rows = latest['rows']
historical = [r['name'] for r in old['rows']]
accelerators = ['deque_ops', 'datetime_ops', 'pickle_bench']
geomean = lambda values: math.exp(statistics.mean(math.log(value) for value in values))
report = {'purpose': __doc__, 'historical_commit': revision, 'historical_report_path': baseline_path,
          'historical_report_sha256': hashlib.sha256(old_bytes).hexdigest(),
          'latest_report_path': str(latest_path), 'latest_report_sha256': hashlib.sha256(latest_path.read_bytes()).hexdigest(),
          'latest_binary': latest['binaries']['new'], 'rows': {}, 'cohorts': {}}
for row in old['rows']:
    name = row['name']
    source_path = 'crates/weavepy-bench/fixtures/' + name + '.py'
    old_source = subprocess.check_output(['git', 'show', revision + ':' + source_path])
    assert old_source == Path(source_path).read_bytes(), name
    assert row['work'] == rows[name]['work'], name
    report['rows'][name] = {'fixture_sha256': hashlib.sha256(old_source).hexdigest(),
                           'work': row['work'], 'historical_ratio': row['ratio'],
                           'latest_ratio_of_medians': rows[name]['new']['ns'] / rows[name]['cpython']['ns'],
                           'latest_median_paired_ratio': rows[name]['cpython_comparisons']['jit']['ns']}
for name, names in [('historical_21_including_startup', historical),
                    ('original_20_workloads', [n for n in historical if n != 'startup']),
                    ('complete_23_workloads', [n for n in rows if n != 'startup']),
                    ('all_24_including_startup', list(rows))]:
    report['cohorts'][name] = {'fixtures': names, 'count': len(names),
        'latest_ratio_of_medians': geomean([rows[n]['new']['ns'] / rows[n]['cpython']['ns'] for n in names]),
        'latest_median_paired_ratios': geomean([rows[n]['cpython_comparisons']['jit']['ns'] for n in names])}
report['historical_headline'] = geomean([r['ratio'] for r in old['rows']])
assert abs(report['historical_headline'] - old['geomean_ratio']) < 1e-12
report['accelerator_ratios'] = {n: rows[n]['cpython_comparisons']['jit']['ns'] for n in accelerators}
(out / 'historical-baseline.json').write_bytes(old_bytes)
(out / 'summary.json').write_text(json.dumps(report, indent=2) + '\n')
(out / 'reconcile.py').write_bytes(Path(__file__).read_bytes())
print(json.dumps({k:v for k,v in report.items() if k not in ('rows',)}, indent=2))
