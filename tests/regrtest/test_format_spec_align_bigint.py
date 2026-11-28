"""Format specs: alignment, and big ints with float presentation types.

Padding and alignment (`>`, `<`, `^`, `=`, fill characters, widths the
value already meets) give CPython's strings, and a big int formatted with
`f`, `e`, `%` or `g` converts as `float(n)` does, raising `OverflowError`
when it can't. Run past the compile threshold.
"""


def run(n):
    for i in range(n):
        assert "{:>8}|{:<6}|{:^7}|{:*^9}".format("ab", 7, "x", -3) == "      ab|7     |   x   |***-3****"
        assert format(-42, "=+8") == "-     42" and format(255, "#010x") == "0x000000ff"
        assert format("héllo", ">7") == "  héllo" and format("toolong", ">3") == "toolong"
        assert format(10**20, ".2f") == "100000000000000000000.00"
        assert format(-10**20, "e") == "-1.000000e+20"
        assert "%.1f" % 10**20 == "100000000000000000000.0"
        assert f"{10**18:%}" == "100000000000000000000.000000%"
        assert format(10**20, "g") == "1e+20"


run(2000)
try:
    format(10**400, ".2f")
except OverflowError:
    pass
else:
    raise AssertionError("a huge int as a float")
print("ok")
