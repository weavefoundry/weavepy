"""Container reprs append plain items in place.

Ints (both extremes), bools, `None` and printable ASCII strings without a
quote or backslash are written directly; strings that need escaping or
the other quote, non-ASCII text, floats and nested containers render as
before.
"""

items = ["", "a b", "it's", 'say "hi"', "back\\slash", "tab\t", "é", "\x7f",
         "~", " ", "\n", "x'y\"z", 0, -1, 2**63 - 1, -2**63, 10**20, True,
         False, None, 1.5, -0.0, (1,), [], {}]
expected_list = (
    "['', 'a b', \"it's\", 'say \"hi\"', 'back\\\\slash', 'tab\\t', 'é', "
    "'\\x7f', '~', ' ', '\\n', 'x\\'y\"z', 0, -1, 9223372036854775807, "
    "-9223372036854775808, 100000000000000000000, True, False, None, 1.5, "
    "-0.0, (1,), [], {}]"
)
assert repr(items) == expected_list, repr(items)
assert repr(tuple(items)) == "(" + expected_list[1:-1] + ")"
assert str({"k": 1, 2: None, "it's": False}) == "{'k': 1, 2: None, \"it's\": False}"
assert repr((5,)) == "(5,)" and repr([]) == "[]" and repr({}) == "{}"
print("ok")
