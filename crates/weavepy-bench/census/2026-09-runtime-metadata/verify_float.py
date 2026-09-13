"""Compare seeded float spellings with CPython and the checkpoint binary."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import random
import struct
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[4]
SEED = 0xF10A72026
DRIVER = '''
import json, struct, sys
with open(sys.argv[1]) as source:
    for line in source:
        bits = int(line, 16)
        value = struct.unpack(">d", struct.pack(">Q", bits))[0]
        print("|".join((repr(value), str(value), repr(complex(value, -value)),
                        json.dumps([value, -value]))))
'''


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", required=True)
    parser.add_argument("--new", required=True)
    parser.add_argument("--python", default="python3.14")
    parser.add_argument("--samples", type=int, default=50000)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    rng = random.Random(SEED)
    bits = [rng.getrandbits(64) for _ in range(args.samples)]
    # Exercise both signs at every exponent, and neighbors of notation and
    # subnormal boundaries, in addition to uniformly sampled binary64 values.
    for exponent in range(2048):
        for mantissa in (0, 1, (1 << 52) - 1):
            for sign in (0, 1 << 63):
                bits.append(sign | (exponent << 52) | mantissa)
    for value in (1e-5, 1e-4, 1e15, 1e16):
        center = struct.unpack(">Q", struct.pack(">d", value))[0]
        for offset in range(-5, 6):
            bits.extend((center + offset, (center + offset) | (1 << 63)))
    outputs = {}
    env = {**os.environ, "WEAVEPY_STDLIB_CACHE": str(ROOT / "target/performance-stdlib-cache")}
    with tempfile.TemporaryDirectory() as temporary:
        data = Path(temporary) / "bits.txt"
        data.write_text("".join(f"{value:016x}\n" for value in bits))
        for name, binary in (("cpython", args.python), ("base", args.base), ("new", args.new)):
            output = subprocess.run([binary, "-c", DRIVER, str(data)], env=env,
                                    check=True, capture_output=True, text=True, timeout=300).stdout
            outputs[name] = output.splitlines()
            assert len(outputs[name]) == len(bits), (name, len(outputs[name]), len(bits))
            print(name, "checked", len(bits), "values", flush=True)
    differences = {}
    for left, right in (("new", "base"), ("base", "cpython"), ("new", "cpython")):
        rows = [{"bits": f"{bits[i]:016x}", left: a, right: b}
                for i, (a, b) in enumerate(zip(outputs[left], outputs[right], strict=True)) if a != b]
        differences[left + "_vs_" + right] = {"count": len(rows), "examples": rows[:20]}
    report = {"seed": SEED, "values": len(bits), "differences": differences,
              "output_sha256": {name: hashlib.sha256("\n".join(rows).encode()).hexdigest()
                                for name, rows in outputs.items()},
              "binaries": {name: {"path": path, "sha256": hashlib.sha256(Path(path).read_bytes()).hexdigest()}
                           for name, path in (("base", args.base), ("new", args.new))}}
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(report, indent=2) + "\n")
    print({name: row["count"] for name, row in differences.items()}, flush=True)
    # Baseline differences remain visible, but the candidate must now match
    # CPython exactly, including shortest-decimal ties the baseline got wrong.
    raise SystemExit(bool(differences["new_vs_cpython"]["count"]))


if __name__ == "__main__":
    main()
