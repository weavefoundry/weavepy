"""Ecosystem probe: pyyaml — safe_load/safe_dump round-trip including
anchors/aliases, nested structures, and multi-document streams."""

import yaml

# anchors + aliases resolve to shared values
doc = """
base: &b
  retries: 3
  timeout: 10
service_a:
  <<: *b
  name: a
service_b:
  <<: *b
  timeout: 30
"""
data = yaml.safe_load(doc)
assert data["service_a"]["retries"] == 3
assert data["service_a"]["name"] == "a"
assert data["service_b"]["timeout"] == 30

# dump -> load round-trip
obj = {
    "name": "weave",
    "versions": [1, 2, 3],
    "nested": {"pi": 3.14, "ok": True, "none": None},
}
assert yaml.safe_load(yaml.safe_dump(obj)) == obj

# multi-document stream
docs = list(yaml.safe_load_all("---\na: 1\n---\nb: 2\n"))
assert docs == [{"a": 1}, {"b": 2}], docs

# scalar typing
assert yaml.safe_load("x: 2024-01-02").__class__ is dict
loaded = yaml.safe_load("[1, 2.5, yes, 'str', null]")
assert loaded == [1, 2.5, True, "str", None], loaded

print("pyyaml ok", yaml.__version__, "with_libyaml:", yaml.__with_libyaml__)
