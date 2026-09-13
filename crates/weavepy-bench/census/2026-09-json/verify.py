"""Generate seeded CPython JSON oracles and check both WeavePy execution modes."""

import argparse
import json
import os
from pathlib import Path
import random
import subprocess
import sys
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--weavepy", required=True)
    args = parser.parse_args()
    assert sys.implementation.name == "cpython", "Generate oracles with CPython"
    rng = random.Random(287391)
    atoms = [None, True, False, 0, -1, 2**63, -2**63-1, 1.25, -0.0, 1e-300,
             1e300, "", "ascii", 'quote"\\\n', "café Σ 😀", "\ud800", "\udfff", "\ud800\udc00"]

    def value(depth=0):
        if depth > 3 or rng.randrange(3) == 0:
            return rng.choice(atoms)
        if rng.randrange(2):
            return [value(depth + 1) for _ in range(rng.randrange(5))]
        return {str(i) + rng.choice(["key", "é", "\ud800"]): value(depth + 1)
                for i in range(rng.randrange(5))}

    encodings, decodings, errors = [], [], []
    for i in range(250):
        obj = value()
        options = dict(ensure_ascii=bool(i % 2), sort_keys=bool(i % 3),
                       indent=rng.choice([None, 0, 2, "\t", "\ud800"]))
        if i % 4 == 0:
            options["separators"] = rng.choice([(",", ":"), (" | ", " -> "), ("\ud800", "\udfff")])
        encodings.append((obj, options, json.dumps(obj, **options)))
        text = json.dumps(obj, ensure_ascii=bool(i % 2))
        decodings.append((text, json.dumps(json.loads(text), sort_keys=True)))
        pos = rng.randrange(len(text))
        broken = text[:pos] + rng.choice(["]", ":", "\x01", "\u03a3"]) + text[pos + 1:]
        try:
            decoded = json.loads(broken)
        except json.JSONDecodeError as error:
            errors.append((broken, error.pos, error.lineno, error.colno))
        else:
            decodings.append((broken, json.dumps(decoded, sort_keys=True)))

    source = (
        "import json\nencodings = " + repr(encodings) +
        "\ndecodings = " + repr(decodings) + "\nerrors = " + repr(errors) + '''
for i, (obj, options, expected) in enumerate(encodings):
    actual = json.dumps(obj, **options)
    assert actual == expected, ("encoding", i, repr(actual), repr(expected))
for i, (text, expected) in enumerate(decodings):
    actual = json.dumps(json.loads(text), sort_keys=True)
    assert actual == expected, ("decoding", i, actual, expected)
for i, (text, pos, line, col) in enumerate(errors):
    try:
        json.loads(text)
    except json.JSONDecodeError as error:
        assert (error.pos, error.lineno, error.colno) == (pos, line, col), ("error", i)
    else:
        raise AssertionError(("invalid input accepted", i))
'''
    )
    with tempfile.TemporaryDirectory(prefix="weavepy-json-oracle-") as directory:
        script = Path(directory) / "oracle.py"
        script.write_text(source)
        for name, flags, jit in [("JIT", [], "1"), ("interpreter", [], "0"),
                                 ("free threading", ["-X", "gil=0"], "1")]:
            subprocess.run([args.weavepy, *flags, str(script)], check=True,
                           env={**os.environ, "WEAVEPY_JIT": jit}, timeout=60)
            print(name, "passed:", len(encodings), "encodings,", len(decodings),
                  "decodings,", len(errors), "error positions", flush=True)


if __name__ == "__main__":
    main()
