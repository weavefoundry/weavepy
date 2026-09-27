"""Unproved native reads must resume before a Python attribute callback."""

import sys

seen = []


class PropertyProbe:
    @property
    def value(self):
        frame = sys._getframe(1)
        seen.append((frame.f_code.co_name, frame.f_lineno,
                     frame.f_locals.get("i"), frame.f_locals.get("marker")))
        return 7


def property_loop(root, n):
    marker = "property-loop"
    total = 0
    for i in range(n):
        total += root.value
    return total


root = PropertyProbe()
for _ in range(2):
    assert property_loop(root, 160) == 1120
expected = [("property_loop", property_loop.__code__.co_firstlineno + 4,
             i, "property-loop") for _ in range(2) for i in range(160)]
assert seen == expected, (seen[:3], seen[-3:], expected[:3])

# A callback can modify its caller's live locals through the frame proxy.
class Rewrite:
    @property
    def value(self):
        caller = sys._getframe(1)
        caller.f_locals["marker"] = "changed"
        return 1


def write_through(root, n):
    marker = "original"
    for i in range(n):
        result = root.value
    return marker, result


for _ in range(2):
    assert write_through(Rewrite(), 160) == ("changed", 1)

class Broken:
    def __init__(self):
        self.calls = 0

    @property
    def value(self):
        self.calls += 1
        caller = sys._getframe(1)
        assert caller.f_code.co_name == "property_loop"
        assert caller.f_locals["marker"] == "property-loop"
        raise ValueError("attribute callback failed")


broken = Broken()
try:
    property_loop(broken, 3)
except ValueError as exc:
    assert str(exc) == "attribute callback failed"
    assert broken.calls == 1
    names = []
    tb = exc.__traceback__
    while tb is not None:
        names.append(tb.tb_frame.f_code.co_name)
        tb = tb.tb_next
    assert names[-2:] == ["property_loop", "value"], names
else:
    raise AssertionError("callback didn't raise")

print("dynamic attribute callback frames: ok")
