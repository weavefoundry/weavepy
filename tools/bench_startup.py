#!/usr/bin/env python3
"""Compare startup and import elapsed time, CPU time, and peak RSS.

Use otherwise idle hardware and release binaries. Alternate paired process
order, discard one warmup cycle, and verify that frozen caches stay unchanged.
Both WeavePy execution modes use separate caches for each binary.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import statistics
import subprocess
import sys
import tempfile
import time

import bench_compare


def measure(binary, jit, command, cache):
    env = os.environ.copy()
    if jit is None:
        env.pop('WEAVEPY_JIT', None)
    else:
        env['WEAVEPY_JIT'] = jit
        env['WEAVEPY_FROZEN_CACHE'] = str(cache.resolve())
    with tempfile.TemporaryFile() as output:
        start = time.perf_counter_ns()
        child = subprocess.Popen([binary, *command], env=env,
                                 stdin=subprocess.DEVNULL, stdout=output,
                                 stderr=subprocess.STDOUT)
        _, status, usage = os.wait4(child.pid, 0)
        elapsed = time.perf_counter_ns() - start
        child.returncode = os.waitstatus_to_exitcode(status)
        if child.returncode:
            output.seek(0)
            raise RuntimeError(output.read().decode(errors='replace'))
    return {
        'wall_ns': elapsed,
        'cpu_ns': round((usage.ru_utime + usage.ru_stime) * 1e9),
        'rss_bytes': usage.ru_maxrss * (1 if sys.platform == 'darwin' else 1024),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--base', required=True)
    parser.add_argument('--new', required=True)
    parser.add_argument('--python', default='python3.14')
    parser.add_argument('--samples', type=int, default=31)
    parser.add_argument('--out', required=True, type=Path)
    parser.add_argument('--frozen-cache-root', required=True, type=Path)
    args = parser.parse_args()
    if args.samples < 1:
        parser.error('--samples must be positive')
    args.frozen_cache_root.mkdir(parents=True, exist_ok=False)
    variants = [
        ('base', args.base, '1'), ('new', args.new, '1'),
        ('base_interp', args.base, '0'), ('new_interp', args.new, '0'),
        ('cpython', args.python, None),
    ]
    hashes = {label: hashlib.sha256(Path(binary).read_bytes()).hexdigest()
              for label, binary in [('base', args.base), ('new', args.new)]}
    original_cache = os.environ.get('WEAVEPY_FROZEN_CACHE')
    try:
        for label, binary, _ in variants[:-1]:
            os.environ['WEAVEPY_FROZEN_CACHE'] = str(
                (args.frozen_cache_root / label).resolve())
            bench_compare.verify_runtime(binary)
    finally:
        if original_cache is None:
            os.environ.pop('WEAVEPY_FROZEN_CACHE', None)
        else:
            os.environ['WEAVEPY_FROZEN_CACHE'] = original_cache
    report = {
        'platform': platform.platform(), 'samples': args.samples,
        'execution_context': bench_compare.execution_context(),
        'binaries': {label: {'path': binary, 'sha256': hashes[label]}
                     for label, binary in [('base', args.base), ('new', args.new)]},
        'python': subprocess.check_output([args.python, '--version'], text=True).strip(),
        'rows': {},
    }
    for name, command in [
        ('pass', ['-c', 'pass']),
        ('no_site', ['-S', '-c', 'pass']),
        ('isolated', ['-I', '-c', 'pass']),
        ('imports', ['-c', 'import json, datetime, collections, pathlib']),
    ]:
        samples = {label: [] for label, _, _ in variants}
        for cycle in range(args.samples + 1):
            order = variants if cycle % 2 == 0 else reversed(variants)
            for label, binary, jit in order:
                sample = measure(binary, jit, command, args.frozen_cache_root / label)
                if cycle:
                    samples[label].append(sample)
            if cycle == 0:
                before = bench_compare.frozen_cache_snapshot(args.frozen_cache_root)
        after = bench_compare.frozen_cache_snapshot(args.frozen_cache_root)
        assert before == after, name + ': frozen cache changed during measurement'
        row = {label: {
            'samples': values,
            **{key: statistics.median(value[key] for value in values) for key in values[0]},
        } for label, values in samples.items()}
        row.update(command=command, comparisons={}, cpython_comparisons={},
                   frozen_cache={'before': before, 'after': after, 'unchanged': True})
        for mode, suffix in [('jit', ''), ('interp', '_interp')]:
            row['comparisons'][mode] = bench_compare.relative_metrics(
                samples['new' + suffix], samples['base' + suffix])
            row['cpython_comparisons'][mode] = bench_compare.relative_metrics(
                samples['new' + suffix], samples['cpython'])
        report['rows'][name] = row
        args.out.parent.mkdir(parents=True, exist_ok=True)
        args.out.write_text(json.dumps(report, indent=2) + '\n')
        print(name, row['comparisons'], row['cpython_comparisons'], flush=True)
    for label, binary in [('base', args.base), ('new', args.new)]:
        assert hashlib.sha256(Path(binary).read_bytes()).hexdigest() == hashes[label]


if __name__ == '__main__':
    main()
