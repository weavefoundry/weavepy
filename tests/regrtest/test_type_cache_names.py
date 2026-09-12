"""Attribute caches compare names exactly, including negative lookups."""

# Equal-length FxHasher collisions on little-endian targets. Cache correctness
# must not depend on whether these spellings happen to share a hash elsewhere.
present = 'AAAAAAAAAAAAAAAA'
missing = '9d<lxa!kH> PIyLn'


class Parent:
    pass


setattr(Parent, present, 42)


class Child(Parent):
    pass


obj = Child()
for _ in range(100):
    assert getattr(obj, missing, None) is None
    assert getattr(obj, present) == 42
    assert getattr(Child, missing, None) is None
    assert getattr(Child, present) == 42


class Descriptor:
    def __get__(self, obj, owner):
        return 73


setattr(Parent, present, Descriptor())
for _ in range(100):
    assert getattr(obj, missing, None) is None
    assert getattr(obj, present) == 73
    assert getattr(Child, present) == 73

setattr(Parent, missing, 99)
assert getattr(obj, missing) == 99
assert getattr(obj, present) == 73
delattr(Parent, missing)
assert getattr(obj, missing, None) is None
assert getattr(obj, present) == 73
print('ok')
