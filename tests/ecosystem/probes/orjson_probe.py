"""Ecosystem probe: orjson (PyO3, version-specific wheel) — dumps/loads
round-trip incl. dataclasses, datetimes, and option flags."""

import dataclasses
import datetime
import uuid

import orjson

# basic round-trip
obj = {"a": [1, 2.5, None, True], "s": "héllo", "nested": {"k": "v"}}
data = orjson.dumps(obj)
assert isinstance(data, bytes)
assert orjson.loads(data) == obj

# native datetime / uuid serialization
now = datetime.datetime(2026, 7, 20, 12, 0, 0)
out = orjson.loads(orjson.dumps({"t": now}))
assert out["t"] == "2026-07-20T12:00:00", out
u = uuid.UUID("12345678-1234-5678-1234-567812345678")
assert orjson.loads(orjson.dumps(u)) == str(u)


# dataclass serialization
@dataclasses.dataclass
class Point:
    x: int
    y: int


assert orjson.loads(orjson.dumps(Point(1, 2))) == {"x": 1, "y": 2}

# option flags
assert orjson.dumps({"b": 1, "a": 2}, option=orjson.OPT_SORT_KEYS) == b'{"a":2,"b":1}'
pretty = orjson.dumps({"a": 1}, option=orjson.OPT_INDENT_2)
assert pretty == b'{\n  "a": 1\n}', pretty

# error surface
try:
    orjson.loads(b"{invalid")
except orjson.JSONDecodeError:
    pass
else:
    raise AssertionError("JSONDecodeError not raised")

print("orjson ok", orjson.__version__)
