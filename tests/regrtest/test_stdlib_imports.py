"""Smoke test: import a swath of stdlib modules and exercise their headline APIs."""

import math
assert math.sqrt(16) == 4
assert math.pi > 3.14 and math.pi < 3.15
assert math.factorial(5) == 120
assert math.gcd(12, 18) == 6

import os
assert os.sep in ("/", "\\")
assert isinstance(os.getcwd(), str)
assert isinstance(os.environ.get("PATH", ""), str)

import sys
assert isinstance(sys.path, list)
assert isinstance(sys.argv, list)
assert sys.maxsize > 0

import json
# Note: WeavePy's `json.dumps` builtin doesn't accept keyword
# arguments yet; we test the positional form here. Also note that
# WeavePy's `dumps` emits a tighter representation than CPython
# (no spaces after separators), matching the spec but differing
# from CPython's defaults.
assert json.loads('{"a": 1}') == {"a": 1}
assert json.loads(json.dumps([1, 2, 3])) == [1, 2, 3]

import re
m = re.match(r"(\d+)-(\d+)", "12-34")
assert m and m.groups() == ("12", "34")
assert re.findall(r"\d+", "a1 b22 c333") == ["1", "22", "333"]
assert re.sub(r"\d+", "X", "a1 b22") == "aX bX"

import itertools
assert list(itertools.chain([1, 2], [3, 4])) == [1, 2, 3, 4]
assert list(itertools.islice(itertools.count(), 5)) == [0, 1, 2, 3, 4]
assert list(itertools.combinations("abc", 2)) == [("a", "b"), ("a", "c"), ("b", "c")]

import functools
@functools.lru_cache(maxsize=8)
def fib(n):
    return n if n < 2 else fib(n - 1) + fib(n - 2)

assert fib(20) == 6765

import collections
c = collections.Counter("mississippi")
# Tie-breaking between equally-frequent items varies; sort defensively.
assert sorted(c.most_common(2)) == [("i", 4), ("s", 4)]
d = collections.defaultdict(list)
d["a"].append(1)
d["a"].append(2)
assert d["a"] == [1, 2]
od = collections.OrderedDict()
od["a"] = 1
od["b"] = 2
assert list(od) == ["a", "b"]

import pathlib
p = pathlib.Path("/tmp")
assert p.name == "tmp"
assert str(p) == "/tmp"

import enum
class Color(enum.Enum):
    RED = 1
    GREEN = 2

assert Color.RED.value == 1
assert Color(1) is Color.RED
assert Color.GREEN.name == "GREEN"

import datetime
dt = datetime.datetime(2024, 1, 1, 12, 0)
assert dt.year == 2024
assert dt.isoformat() == "2024-01-01T12:00:00"
delta = datetime.timedelta(days=1)
assert (dt + delta).day == 2

import io
buf = io.StringIO()
buf.write("hello\n")
buf.write("world\n")
assert buf.getvalue() == "hello\nworld\n"

import contextlib
@contextlib.contextmanager
def temp_value(d, k, v):
    old = d.get(k)
    d[k] = v
    try:
        yield
    finally:
        if old is None:
            del d[k]
        else:
            d[k] = old

box = {}
with temp_value(box, "x", 1):
    assert box["x"] == 1
assert "x" not in box
