"""Large compiler pools preserve bindings, constant types, and signed zero."""

import math

source = "\n".join("name_%d = %d" % (i, i) for i in range(512))
source += "\nresult = [" + ",".join("name_%d" % i for i in range(512)) + "]\n"
source += "values = (0, 0.0, -0.0, False, True, 1, 1.0, b'x', 'x')\n"
source += "\n".join("def f%d(): return name_%d" % (i, i) for i in range(80))
namespace = {}
exec(compile(source, "<indexed-compiler-test>", "exec"), namespace)
assert namespace["result"] == list(range(512))
values = namespace["values"]
assert tuple(type(value) for value in values) == (
    int, float, float, bool, bool, int, float, bytes, str,
)
assert math.copysign(1, values[1]) == 1
assert math.copysign(1, values[2]) == -1
for i in range(80):
    function = namespace["f%d" % i]
    assert function() == i
    assert function.__name__ == "f%d" % i

# Names and constants remain reusable after a pool crosses its index threshold.
for name in ("name_0", "name_31", "name_32", "name_511"):
    assert namespace[name] == int(name[5:])
