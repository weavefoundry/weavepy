"""Ecosystem probe: attrs — define/validate/asdict/frozen classes."""

import attr
import attrs


@attrs.define
class Point:
    x: int
    y: int = 0


p = Point(1, 2)
assert (p.x, p.y) == (1, 2)
assert attrs.asdict(p) == {"x": 1, "y": 2}
assert Point(1) == Point(1, 0)
assert Point(1, 2) != Point(2, 1)
assert "Point(x=1, y=2)" == repr(p)

# evolve
q = attrs.evolve(p, y=9)
assert (q.x, q.y) == (1, 9)


# frozen classes raise on mutation
@attrs.frozen
class Frozen:
    v: int


f = Frozen(5)
try:
    f.v = 6
except attrs.exceptions.FrozenInstanceError:
    pass
else:
    raise AssertionError("frozen instance was mutable")


# validators
@attrs.define
class Bounded:
    n: int = attrs.field(validator=attrs.validators.ge(0))


Bounded(3)
try:
    Bounded(-1)
except ValueError:
    pass
else:
    raise AssertionError("validator did not fire")

# classic attr.s API
@attr.s
class Legacy(object):
    a = attr.ib(default=1)


assert Legacy().a == 1

print("attrs ok", attr.__version__)
