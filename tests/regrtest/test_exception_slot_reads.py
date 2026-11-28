"""`e.args` and an exception's other pseudo-slots read from compiled code.

An exception's `args` lives in its slot storage, read through the class's
slot descriptor. Compiled code reads it there once a site finds the class
resolving the name to that descriptor: a subclass that replaces `args`
with a property, or an instance whose `args` is reassigned, must still
read what Python would. Run it past the compile threshold.
"""


class E(Exception):
    pass


class Prop(Exception):
    @property
    def args(self):
        return ("prop",)


def read(e):
    return e.args


e = E(1, 2)
for _ in range(3000):
    assert read(e) == (1, 2)
    assert read(ValueError("v")) == ("v",)
    assert read(Prop(5)) == ("prop",)

e.args = ("new",)
for _ in range(100):
    assert read(e) == ("new",)

E.args = property(lambda self: "shadowed")
for _ in range(100):
    assert read(e) == "shadowed", read(e)
