"""Measure explicit full-collection pause distributions in isolated processes."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import runpy
import statistics
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[4]
KERNEL = '''
import gc
import json
import time
import weakref
class Node:
    __slots__ = ('other', '__weakref__')
def cycle(count):
    nodes = [Node() for _ in range(count)]
    for i, node in enumerate(nodes):
        node.other = nodes[(i + 1) % count]
    return nodes
mode = MODE
count = 10000
pauses = []
gc.disable()
gc.collect(2)
if mode in ('rooted', 'frozen'):
    roots = cycle(count)
    if mode == 'frozen':
        gc.freeze()
        assert gc.get_freeze_count() >= count
for iteration in range(52):
    if mode == 'unreachable':
        nodes = cycle(count)
        references = [weakref.ref(node) for node in nodes]
        del nodes
    start = time.perf_counter_ns()
    gc.collect(2)
    elapsed = time.perf_counter_ns() - start
    if iteration:
        pauses.append(elapsed)
    if mode == 'unreachable':
        assert all(reference() is None for reference in references)
    else:
        assert roots[-1].other is roots[0]
if mode == 'frozen':
    gc.unfreeze()
    assert gc.get_freeze_count() == 0
assert len(pauses) == 51
print('PAUSES', json.dumps(pauses))
'''


def measure(binary, mode):
    source = KERNEL.replace('MODE', repr(mode))
    env = {**os.environ, 'WEAVEPY_JIT': '0'}
    with tempfile.TemporaryFile() as output:
        start = time.perf_counter_ns()
        child = subprocess.Popen([binary, '-c', source], env=env,
                                 stdin=subprocess.DEVNULL, stdout=output,
                                 stderr=subprocess.STDOUT)
        _, status, usage = os.wait4(child.pid, 0)
        wall = time.perf_counter_ns() - start
        child.returncode = os.waitstatus_to_exitcode(status)
        output.seek(0)
        text = output.read().decode(errors='replace')
    if child.returncode:
        raise RuntimeError(f'{binary}: {text}')
    payload = next((line[7:] for line in text.splitlines() if line.startswith('PAUSES ')), None)
    if payload is None:
        raise RuntimeError(f'Missing pause output: {text}')
    pauses = json.loads(payload)
    assert len(pauses) == 51 and all(isinstance(value, int) and value >= 0 for value in pauses)
    ordered = sorted(pauses)
    return {'wall_ns': wall, 'cpu_ns': round((usage.ru_utime + usage.ru_stime) * 1e9),
            'rss_bytes': usage.ru_maxrss * (1 if sys.platform == 'darwin' else 1024),
            'pauses_ns': pauses, 'median_pause_ns': statistics.median(pauses),
            # Nearest-rank p95 of 51 samples is the 49th ordered observation.
            'p95_pause_ns': ordered[48], 'max_pause_ns': ordered[-1]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--base', required=True)
    parser.add_argument('--new', required=True)
    parser.add_argument('--python', default='python3.14')
    parser.add_argument('--samples', type=int, default=7)
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    if args.samples < 1:
        parser.error('--samples must be positive')
    verify = runpy.run_path(str(ROOT / 'tools/bench_compare.py'))['verify_runtime']
    verify(args.base)
    verify(args.new)
    variants = [('base', args.base), ('new', args.new), ('cpython', args.python)]
    report = {'platform': platform.platform(), 'jit': False, 'samples': args.samples,
              'description': '51 explicit full-GC pauses per process after one discarded collection; '
                             'automatic collection disabled; 10000-node cyclic graph; '
                             'median, nearest-rank p95, and maximum are within-process statistics.',
              'binaries': {label: {'path': binary, 'bytes': Path(binary).stat().st_size,
                                   'sha256': hashlib.sha256(Path(binary).read_bytes()).hexdigest()}
                           for label, binary in variants[:2]}, 'rows': {}}
    args.out.parent.mkdir(parents=True, exist_ok=True)
    for mode in ['rooted', 'unreachable', 'frozen']:
        samples = {label: [] for label, _ in variants}
        for cycle in range(args.samples + 1):
            for label, binary in variants if cycle % 2 == 0 else reversed(variants):
                result = measure(binary, mode)
                if cycle:
                    samples[label].append(result)
        comparisons = {
            label: {metric: statistics.median(new[metric] / old[metric]
                                              for new, old in zip(samples['new'], samples[label], strict=True))
                    for metric in samples['new'][0] if metric != 'pauses_ns'}
            for label in ['base', 'cpython']}
        report['rows'][mode] = {'samples': samples, 'comparisons': comparisons}
        args.out.write_text(json.dumps(report, indent=2) + '\n')
        print(mode, comparisons, flush=True)


if __name__ == '__main__':
    main()
