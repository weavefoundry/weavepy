"""Interleave the existing eight-thread fixture across binaries and GIL modes."""

import argparse
import json
import os
from pathlib import Path
import re
import statistics
import subprocess
import tempfile
import time


def measure(binary, gil, work):
    with tempfile.TemporaryFile() as output:
        start = time.perf_counter_ns()
        child = subprocess.Popen(
            [binary, '-X', f'gil={gil}', 'crates/weavepy-bench/fixtures/parallel_scaling.py'],
            env={**os.environ, 'WEAVEPY_BENCH_WORK': str(work), 'WEAVEPY_JIT': '1'},
            stdin=subprocess.DEVNULL, stdout=output, stderr=subprocess.STDOUT,
        )
        _, status, usage = os.wait4(child.pid, 0)
        wall = time.perf_counter_ns() - start
        child.returncode = os.waitstatus_to_exitcode(status)
        output.seek(0)
        text = output.read().decode(errors='replace')
    if child.returncode:
        raise RuntimeError(text)
    import sys
    serial = int(re.search(r'WEAVEPY_BENCH_SERIAL_NS=(\d+)', text)[1])
    parallel = int(re.search(r'WEAVEPY_BENCH_PARALLEL_NS=(\d+)', text)[1])
    return {
        'serial_ns': serial, 'parallel_ns': parallel, 'scaling': serial / parallel,
        'wall_ns': wall, 'cpu_ns': round((usage.ru_utime + usage.ru_stime) * 1e9),
        'rss_bytes': usage.ru_maxrss * (1 if sys.platform == 'darwin' else 1024),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--base', required=True)
    parser.add_argument('--new', required=True)
    parser.add_argument('--samples', type=int, default=5)
    parser.add_argument('--work', type=int, default=1000000)
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    if args.samples < 1 or args.work < 1:
        parser.error('--samples and --work must be positive')
    variants = [(f'{label}_{gil}', binary, gil) for gil in (1, 0)
                for label, binary in [('base', args.base), ('new', args.new)]]
    samples = {label: [] for label, _, _ in variants}
    for cycle in range(args.samples + 1):
        for label, binary, gil in variants if cycle % 2 == 0 else reversed(variants):
            value = measure(binary, gil, args.work)
            if cycle:
                samples[label].append(value)
        print('Completed cycle', cycle, flush=True)
    comparisons = {
        str(gil): {
            key: statistics.median(new[key] / base[key] for new, base in
                                   zip(samples[f'new_{gil}'], samples[f'base_{gil}'], strict=True))
            for key in samples[f'new_{gil}'][0]
        } for gil in (1, 0)
    }
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps({'work': args.work, 'samples': samples,
                                    'comparisons': comparisons}, indent=2) + '\n')
    print(comparisons, flush=True)


if __name__ == '__main__':
    main()
