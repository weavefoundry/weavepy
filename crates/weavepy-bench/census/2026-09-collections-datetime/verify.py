"""Generate CPython arithmetic oracles and check every WeavePy execution mode."""

import argparse
from datetime import date, datetime, timedelta, timezone
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
    parser.add_argument("--base", required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    assert sys.implementation.name == "cpython"
    rng = random.Random(4177)
    cases = []
    atoms = [0, 1, -1, True, False, 0.5, -0.5, 1.5, 0.1,
             999999999, -999999999, 10**10, 2**100, 10**100]
    for _ in range(1500):
        values = [rng.choice(atoms) if rng.randrange(4) == 0
                  else rng.randrange(-10000, 10000) for _ in range(7)]
        try:
            result = timedelta(*values)
        except (OverflowError, TypeError) as error:
            expected = type(error).__name__
        else:
            expected = (result.days, result.seconds, result.microseconds)
        cases.append((values, expected))
    calendars = []
    for _ in range(500):
        ordinal = rng.randrange(2, date.max.toordinal() - 2)
        start = datetime.combine(date.fromordinal(ordinal), datetime.min.time(),
                                 tzinfo=timezone.utc)
        micros = rng.randrange(-86400000000, 86400000000)
        result = start + timedelta(microseconds=micros)
        calendars.append((ordinal, micros, result.isoformat(), result.weekday()))
    source = (
        "from datetime import date, datetime, timedelta, timezone\nimport json\n"
        "cases = " + repr(cases) + "\ncalendars = " + repr(calendars) + '''
results = []
for values, expected in cases:
    try:
        result = timedelta(*values)
    except (OverflowError, TypeError) as error:
        actual = type(error).__name__
    else:
        actual = (result.days, result.seconds, result.microseconds)
    results.append(actual)
for ordinal, micros, text, weekday in calendars:
    start = datetime.combine(date.fromordinal(ordinal), datetime.min.time(),
                             tzinfo=timezone.utc)
    delta = timedelta(microseconds=micros)
    result = start + delta
    assert result.isoformat() == text, (ordinal, micros, result, text)
    assert result.weekday() == weekday
    assert result - start == delta
print(json.dumps(results))
'''
    )
    with tempfile.TemporaryDirectory(prefix="weavepy-datetime-oracle-") as directory:
        script = Path(directory) / "oracle.py"
        script.write_text(source)
        expected = json.loads(json.dumps([value for _, value in cases]))
        report = {}
        for name, flags, jit in [("JIT", [], "1"), ("interpreter", [], "0"),
                                 ("free threading", ["-X", "gil=0"], "1")]:
            outputs = []
            for binary in (args.base, args.weavepy):
                result = subprocess.run(
                    [binary, *flags, str(script)], check=True, capture_output=True,
                    text=True, env={**os.environ, "WEAVEPY_JIT": jit}, timeout=120)
                outputs.append(json.loads(result.stdout))
            baseline, candidate = outputs
            introduced = [i for i, value in enumerate(candidate)
                          if value != expected[i] and value != baseline[i]]
            mismatches = [dict(components=cases[i][0], cpython=expected[i],
                               baseline=baseline[i], candidate=value)
                          for i, value in enumerate(candidate) if value != expected[i]]
            report[name] = dict(cases=len(cases), calendar_cases=len(calendars),
                                introduced_mismatches=introduced,
                                existing_mismatches=mismatches)
            args.out.write_text(json.dumps(report, indent=2) + "\n")
            assert not introduced, (name, introduced)
            print(name, "passed:", len(cases), "timedelta cases and",
                  len(calendars), "calendar cases;", len(mismatches),
                  "unchanged CPython differences", flush=True)


if __name__ == "__main__":
    main()
