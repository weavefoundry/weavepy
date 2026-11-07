"""`hasattr(cls, name)` and `getattr(cls, name, default)` of a name no
class on the MRO has, where a base is a builtin (`int`) so the miss is
remembered per class version rather than proven, from compiled code: an
attribute set later on the class or on a base, or on the metaclass-free
class's builtin-based subclass, is found at once."""


class Op(int):
    pass


class SubOp(Op):
    pass


def probe(cls):
    return hasattr(cls, "_asyncio_future_blocking"), getattr(cls, "flag", "missing")


# Past the frame JIT's threshold, so later rounds run compiled code.
for i in range(3000):
    assert probe(Op) == (False, "missing")
    assert probe(SubOp) == (False, "missing")
    assert hasattr(type(Op(3)), "bit_length")

Op.flag = "on Op"
assert probe(Op) == (False, "on Op")
assert probe(SubOp) == (False, "on Op")
SubOp._asyncio_future_blocking = False
assert probe(SubOp) == (True, "on Op") and probe(Op) == (False, "on Op")
del Op.flag
assert probe(Op) == (False, "missing") and probe(SubOp) == (True, "missing")
for i in range(3000):
    assert probe(SubOp) == (True, "missing")
print("ok")
