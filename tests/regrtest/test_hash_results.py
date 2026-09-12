"""Hash protocol results are exact machine integers with -1 normalized."""

import gc
import sys
import weakref


class HashValue:
    def __init__(self, value):
        self.value = value

    def __hash__(self):
        return self.value


def expected_hash(value):
    limit = 1 << (sys.hash_info.width - 1)
    result = value if -limit <= value < limit else hash(value)
    return -2 if result == -1 else result


class IntegerResult(int):
    def __int__(self):
        raise AssertionError("hash called an integer conversion hook")

    def __index__(self):
        raise AssertionError("hash called an index hook")

    def __hash__(self):
        raise AssertionError("hash called the returned integer's override")


values = (
    -(2 ** 100), -(2 ** 63) - 1, -(2 ** 63), -(2 ** 61),
    -2, -1, 0, 1, 2 ** 61 - 1, 2 ** 61, 2 ** 62,
    2 ** 63 - 1, 2 ** 63, 2 ** 100 + 111,
)
for value in values:
    expected = expected_hash(value)
    for returned in (value, IntegerResult(value)):
        owner = HashValue(returned)
        result = hash(owner)
        assert type(result) is int
        assert result == expected, (value, result, expected)
        assert owner.__hash__() is returned
for returned in (False, True):
    result = hash(HashValue(returned))
    assert type(result) is int and result == int(returned)


class Meta(type):
    def __hash__(cls):
        return meta_result


class Target(metaclass=Meta):
    pass


for value in values:
    meta_result = IntegerResult(value)
    result = hash(Target)
    assert type(result) is int and result == expected_hash(value)
    assert Meta.__hash__(Target) is meta_result


class HashDescriptor:
    def __get__(self, instance, owner):
        return lambda: descriptor_result


class Described:
    __hash__ = HashDescriptor()


class CallableHash:
    def __call__(self):
        return descriptor_result


class CallableOwner:
    __hash__ = CallableHash()


class StaticOwner:
    @staticmethod
    def __hash__():
        return descriptor_result


class ClassOwner:
    @classmethod
    def __hash__(cls):
        return descriptor_result


for value in values:
    descriptor_result = IntegerResult(value)
    for owner in (Described(), CallableOwner(), StaticOwner(), ClassOwner()):
        result = hash(owner)
        assert type(result) is int and result == expected_hash(value)


class IntegerLike:
    def __int__(self):
        raise AssertionError("hash must reject a non-integer")

    def __index__(self):
        raise AssertionError("hash must reject a non-integer")


for invalid in (None, 1.5, "invalid", [], IntegerLike()):
    descriptor_result = invalid
    meta_result = invalid
    for owner in (HashValue(invalid), Described(), CallableOwner(),
                  StaticOwner(), ClassOwner(), Target):
        try:
            hash(owner)
        except TypeError as error:
            assert str(error) == "__hash__ method should return an integer"
        else:
            raise AssertionError("invalid hash result was accepted")


class EqualMinusTwo:
    def __hash__(self):
        return -1

    def __eq__(self, other):
        return isinstance(other, EqualMinusTwo) or other == -2


key = EqualMinusTwo()
mapping = {key: "value"}
assert mapping[-2] == "value"
assert key in {-2}
frozen = frozenset((key,))
assert frozen == frozenset((-2,))
assert hash(frozen) == hash(frozenset((-2,)))
reference = weakref.ref(key)
assert hash(reference) == -2
del mapping, frozen, key
gc.collect()
assert reference() is None
assert hash(reference) == -2


class Raises:
    def __init__(self):
        self.calls = 0

    def __hash__(self):
        self.calls += 1
        raise ValueError("hash failed")


owner = Raises()
for _ in range(2):
    try:
        {owner: 1}
    except ValueError as error:
        assert str(error) == "hash failed"
    else:
        raise AssertionError("hash exception was suppressed")
assert owner.calls == 2


def expect_error(operation, kind, message):
    try:
        operation()
    except kind as error:
        assert str(error) == message
    else:
        raise AssertionError("key callback exception was suppressed")
    assert {"healthy": 3}["healthy"] == 3


class KeyMapping:
    def __init__(self, keys):
        self.order = keys

    def keys(self):
        return self.order

    def __getitem__(self, key):
        return 1


owner = Raises()
constructors = (
    lambda: {owner: 1},
    lambda: dict([(owner, 1)]),
    lambda: dict((key, 1) for key in [owner]),
    lambda: dict([(owner, 1)], extra=2),
    lambda: dict(KeyMapping([owner])),
    lambda: {key: 1 for key in [owner]},
    lambda: {owner},
    lambda: set([owner]),
    lambda: frozenset([owner]),
    lambda: {key for key in [owner]},
    lambda: {*[owner]},
)
for operation in constructors:
    calls = owner.calls
    expect_error(operation, ValueError, "hash failed")
    assert owner.calls == calls + 1


class EqualityRaises:
    def __hash__(self):
        return 42

    def __eq__(self, other):
        raise ValueError("comparison failed")


owner = EqualityRaises()
for operation in (
    lambda: {owner: 1, 42: 2},
    lambda: dict([(owner, 1), (42, 2)]),
    lambda: dict((key, 1) for key in [owner, 42]),
    lambda: dict(KeyMapping([owner, 42])),
    lambda: {key: 1 for key in [owner, 42]},
    lambda: {owner, 42},
    lambda: set([owner, 42]),
    lambda: frozenset([owner, 42]),
    lambda: {key for key in [owner, 42]},
    lambda: {*[owner, 42]},
):
    expect_error(operation, ValueError, "comparison failed")


class KeywordEqualityRaises:
    def __hash__(self):
        return hash("extra")

    def __eq__(self, other):
        raise ValueError("keyword comparison failed")


keyword_key = KeywordEqualityRaises()
expect_error(lambda: dict([(keyword_key, 1)], extra=2),
             ValueError, "keyword comparison failed")

# An exact dictionary copy reuses stored hashes, even when rehashing a key
# would now fail. The copy retains keys and values but owns its table.
class CopyKey:
    fail = False

    def __hash__(self):
        if self.fail:
            raise AssertionError("copy rehashed a key")
        return 73


copy_key = CopyKey()
copy_value = []
original = {copy_key: copy_value}
copy_key.fail = True
copied = dict(original)
assert len(copied) == 1 and next(iter(copied)) is copy_key
assert next(iter(copied.values())) is copy_value
copied.clear()
assert len(original) == 1

# A temporary integer subclass is released before hash() returns.
events = []


class TemporaryInteger(int):
    def __del__(self):
        events.append("released")


class TemporaryHash:
    def __hash__(self):
        return TemporaryInteger(-1)


gc.collect()
gc.disable()
assert hash(TemporaryHash()) == -2
assert events == ["released"]


class InvalidTemporary:
    def __del__(self):
        events.append("invalid released")


class InvalidTemporaryHash:
    def __hash__(self):
        return InvalidTemporary()


try:
    hash(InvalidTemporaryHash())
except TypeError:
    assert events == ["released", "invalid released"]
else:
    raise AssertionError("invalid temporary hash result was accepted")
gc.enable()
print("ok")
