"""Compare live allocations at exact, disassembly-verified weakref sites.

This is selected live-allocation evidence, not allocation churn, timing, or RSS.
Reported instrumented block sizes remain separate from malloc request sizes.
"""
import gzip
import hashlib
import json
from pathlib import Path
import re

out = Path('target/weakref-shared-keys-allocation-summary.json')
assert not out.exists()
root = Path('target/weakref-shared-keys-machine-code')
code_path = root / 'report.json'
key_path = root / 'key-allocation-sites.json'
init_path = root / 'after.wrapper-keys-init.json'
code = json.loads(code_path.read_text())['binaries']
keys = json.loads(key_path.read_text())
init = json.loads(init_path.read_text())
assert keys['binary_sha256'] == code['before']['sha256']
assert init['binary_sha256'] == code['after']['sha256']
report = {'purpose': __doc__, 'evidence': {
    str(p): hashlib.sha256(p.read_bytes()).hexdigest()
    for p in [code_path, key_path, init_path, Path(__file__)]},
    'variants': {}, 'limits': 'Only direct verified malloc return sites are counted. '
    'Zero-node controls and all selected stacks are retained. Instrumentation '
    'changes allocation sizes. These counts do not establish process RSS or churn.'}
header = re.compile(rb'^(\d+) calls? for (\d+) bytes:')
frame = re.compile(rb'^1\s+\S+\s+0x[0-9a-f]+\s+(\S+) \+ (\d+)')
metrics = ['calls', 'instrumented_reported_bytes', 'inferred_requested_bytes']
for variant, capture_root in [
    ('before', Path('target/gc-lazy-finalization-live-before')),
    ('after', Path('target/weakref-shared-keys-live-after')),
]:
    capture_path = capture_root / 'report.json'
    capture = json.loads(capture_path.read_text())
    assert capture['sha256'] == code[variant]['sha256']
    assert len(capture['rows']) == 8
    lookup = {}
    functions = {'factory': code[variant]['functions']['make_ref_object_with_class']}
    if variant == 'after':
        functions['thread_key_init'] = init
    for category, function in functions.items():
        for site in function['direct_malloc_sites']:
            # Every recorded site loads the constant malloc request directly
            # into w0 immediately before calling the verified malloc stub.
            instruction = site['context'][-3]
            match = re.search(r'\bmov\s+w0, #(0x[0-9a-f]+)', instruction)
            assert match, instruction
            lookup[(function['symbol'].lstrip('_'), site['return_offset'])] = {
                'category': category, 'requested_bytes_each': int(match[1], 16)}
    if variant == 'before':
        for key in keys['rows']:
            site = lookup[(keys['symbol'].lstrip('_'), key['return_offset'])]
            assert site['requested_bytes_each'] == key['requested_bytes']
            site['key'] = key['key']
    result = report['variants'][variant] = {
        'binary_sha256': capture['sha256'], 'capture_root': str(capture_root),
        'capture_report_sha256': hashlib.sha256(capture_path.read_bytes()).hexdigest(),
        'rows': {}}
    for label, row in capture['rows'].items():
        assert row['returncode'] == 0
        selected = []
        current = None
        digest = hashlib.sha256()
        with gzip.open(capture_root / (label + '.history.txt.gz'), 'rb') as stream:
            for raw in stream:
                digest.update(raw)
                match = header.match(raw)
                if match:
                    if current and 'category' in current:
                        selected.append(current)
                    current = {'calls': int(match[1]),
                               'instrumented_reported_bytes': int(match[2]), 'stack': []}
                    continue
                if current is None:
                    continue
                match = frame.match(raw)
                if match:
                    symbol, offset = match[1].decode(), int(match[2])
                    site = lookup.get((symbol.lstrip('_'), offset))
                    if site:
                        current.update(site, symbol=symbol, return_offset=offset)
                        current['inferred_requested_bytes'] = current['calls'] * site['requested_bytes_each']
                if 'category' in current and raw.strip() and len(current['stack']) < 14:
                    current['stack'].append(raw.decode(errors='replace').rstrip())
            if current and 'category' in current:
                selected.append(current)
        assert digest.hexdigest() == row['history_sha256']
        groups = {
            'factory': [s for s in selected if s['category'] == 'factory'],
            'factory_keys': [s for s in selected if 'key' in s],
            'thread_key_init': [s for s in selected if s['category'] == 'thread_key_init'],
        }
        totals = {name: {m: sum(s[m] for s in values) for m in metrics}
                  for name, values in groups.items()}
        result['rows'][label] = {'history_sha256': digest.hexdigest(),
                                'values': row['values'], 'totals': totals,
                                'selected_groups': selected}
        print(variant, label, {k: v['calls'] for k, v in totals.items()}, flush=True)
    result['population_deltas'] = {}
    for kind in ['ordinary', 'finalizer', 'weak_no_callback', 'weak_callback']:
        populated = result['rows'][kind + '_3000']['totals']
        zero = result['rows'][kind + '_0']['totals']
        result['population_deltas'][kind] = {
            group: {m: populated[group][m] - zero[group][m] for m in metrics}
            for group in zero}
out.write_text(json.dumps(report, indent=2) + '\n')
