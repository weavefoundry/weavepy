"""Ecosystem probe: packaging — version/specifier/requirement/tags."""

import packaging
from packaging.requirements import Requirement
from packaging.specifiers import SpecifierSet
from packaging.tags import sys_tags
from packaging.version import Version

# versions
assert Version("1.2.3") < Version("1.10.0")
assert Version("2.0.0rc1") < Version("2.0.0")
v = Version("1.2.3.post4")
assert (v.major, v.minor, v.micro, v.post) == (1, 2, 3, 4)

# specifiers
spec = SpecifierSet(">=1.0,<2.0")
assert Version("1.5") in spec
assert Version("2.0") not in spec
assert list(spec.filter(["0.9", "1.0", "1.9", "2.1"])) == ["1.0", "1.9"]

# requirements
req = Requirement('requests[security]>=2.0; python_version >= "3.8"')
assert req.name == "requests"
assert req.extras == {"security"}
assert str(req.specifier) == ">=2.0"
assert req.marker is not None and req.marker.evaluate()

# tags — the current interpreter must produce a non-empty, plausible set
tags = list(sys_tags())
assert tags, "sys_tags() came back empty"
assert any(t.interpreter.startswith(("cp", "py")) for t in tags)

print("packaging ok", packaging.__version__)
