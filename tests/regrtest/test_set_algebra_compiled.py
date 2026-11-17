"""Set union, intersection, difference and symmetric difference in
compiled code.

Plain keys are combined in place with the result kind following the left
operand; keys with a Python `__eq__` take the full path; in-place
operators mutate the left set. Items are heap objects too. Run it past
the compile threshold.
"""


class E:
    eqs = 0

    def __init__(self, v):
        self.v = v

    def __hash__(self):
        return hash(self.v)

    def __eq__(self, other):
        E.eqs += 1
        return isinstance(other, E) and self.v == other.v


s100 = set(range(100))
words = {"a", "b", "c"}
for i in range(3000):
    assert s100 & {1, 2, 300} == {1, 2}
    assert {1, 2} | {2, 3} == {1, 2, 3} and {1, 2} - {2} == {1} and {1, 2} ^ {2, 3} == {1, 3}
    r = frozenset({1, 2}) & {2, 3}
    assert r == {2} and type(r) is frozenset
    assert type({1} | frozenset({2})) is set
    assert words & {"b", "z"} == {"b"}
    E.eqs = 0
    assert {E(1), E(2)} & {E(2), E(3)} == {E(2)}
    assert E.eqs > 0
    x = {1, 2}
    y = x
    x &= {2}
    assert y is x and x == {2}
