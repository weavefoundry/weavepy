"""`json.loads` and `json.dumps` called from compiled loops run their
native scanner and encoder in the core loop's lane and give the full
call's results: hooks that run Python (`object_hook`, `object_pairs_hook`,
`parse_float`, `parse_int`, `default`), a hook that raises, malformed
documents' `JSONDecodeError` with its position, nested structures, and
`sort_keys`/`indent` encoders."""

import json
from collections import OrderedDict
from decimal import Decimal


class Point:
    def __init__(self, x):
        self.x = x


def run(n):
    out = []
    for i in range(n):
        out.append(json.loads('{"a": [1, 2, {"b": null}], "c": "x%d"}' % i))
        out.append(json.loads('{"p": 1}', object_hook=lambda d: ("hook", d["p"] + i)))
        out.append(json.loads('{"z": 1, "a": 2}', object_pairs_hook=OrderedDict))
        out.append(json.loads("[1.5, 2]", parse_float=Decimal, parse_int=str))
        out.append(json.dumps({"b": i, "a": [True, None]}, sort_keys=True))
        out.append(json.dumps(Point(i), default=lambda o: {"x": o.x}))
        out.append(json.dumps([i, {"k": "v"}], indent=1))
    return out


def expected(n):
    out = []
    for i in range(n):
        out.append({"a": [1, 2, {"b": None}], "c": "x%d" % i})
        out.append(("hook", 1 + i))
        out.append(OrderedDict([("z", 1), ("a", 2)]))
        out.append([Decimal("1.5"), "2"])
        out.append('{"a": [true, null], "b": %d}' % i)
        out.append('{"x": %d}' % i)
        out.append("[\n %d,\n {\n  \"k\": \"v\"\n }\n]" % i)
    return out


for _ in range(4):
    got = run(100)
    assert got == expected(100), next((g, e) for g, e in zip(got, expected(100)) if g != e)


def bad(n):
    seen = 0
    for i in range(n):
        try:
            json.loads('{"a": }')
        except json.JSONDecodeError as e:
            seen += e.pos == 6
        try:
            json.loads('[1, 2')
        except json.JSONDecodeError:
            seen += 1
        try:
            json.loads('{"a": 1}', object_hook=lambda d: 1 / 0)
        except ZeroDivisionError:
            seen += 1
        try:
            json.dumps(object())
        except TypeError:
            seen += 1
    return seen


for _ in range(3):
    assert bad(100) == 400
print("ok")
