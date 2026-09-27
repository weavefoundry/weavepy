#!/usr/bin/env python3
"""Generate a main-module compilation and retained-memory probe.

Example: python3 tools/make_compile_main_probe.py --functions 10000 \
    --output target/performance/main-10000.py

Compare generated probes with bench_compare.py. Process elapsed, CPU, and RSS
include parsing and compilation; the printed workload timer covers execution.
The payload touches each page so its resident memory is included.
"""
import argparse
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--functions", type=int, default=10000)
    parser.add_argument("--payload-mib", type=int, default=16)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if args.functions < 1 or args.payload_mib < 0:
        parser.error("functions must be positive and payload-mib must be nonnegative")
    lines = ["import time", "start = time.perf_counter_ns()"]
    for i in range(args.functions):
        lines.append("def function_%d():\n    return %d + 1" % (i, i))
    lines.extend([
        "payload = bytearray(%d)" % (args.payload_mib * 1024 * 1024),
        "for page in range(0, len(payload), 4096):\n    payload[page] = 123",
        "assert function_0() == 1",
        "assert function_%d() == %d" % (args.functions - 1, args.functions),
        "assert len(payload) == %d" % (args.payload_mib * 1024 * 1024),
        'print("WEAVEPY_BENCH_NS=%d" % (time.perf_counter_ns() - start))',
    ])
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text("\n".join(lines) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
