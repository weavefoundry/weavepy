"""Borrowed getter paths stop before callbacks and retain ordinary call rules."""

import sys


class Properties:
    def __init__(self):
        self._value = 7

    @property
    def value(self):
        return self._value

    @property
    def other(self):
        return 11


class Missing:
    def __init__(self):
        self._value = 7

    def __getattr__(self, name):
        if name == "value":
            return self._value
        if name == "other":
            return self._value + 4
        raise AttributeError(name)


def read(root, n):
    marker = "initial"
    total = 0
    for i in range(n):
        total += root.value
        total += root.other
    return total, marker


# Separate warmed code objects keep later fallback cases independent of the
# retirement budget consumed by earlier callback-heavy cases.
def binding_read(root, n):
    total = 0
    for _ in range(n):
        total += root.value
        total += root.other
    return total


def suspended_read(root, n):
    result = None
    for _ in range(n):
        result = root.value
    return result


def key_read(root, n):
    total = 0
    for _ in range(n):
        total += root.value
        total += root.other
    return total


properties = Properties()
missing = Missing()
for _ in range(2):
    assert read(properties, 1200) == (21600, "initial")
    assert read(missing, 1200) == (21600, "initial")
    assert binding_read(properties, 1200) == 21600
    assert suspended_read(properties, 1200) == 7
    assert key_read(missing, 1200) == 21600

# GETTER MUTATIONS: Rust verifies fast getter hits before this point.

events = []
original_property_code = Properties.value.fget.__code__
original_getattr = Missing.__getattr__


def observing_property(self):
    caller = sys._getframe(1)
    events.append((caller.f_code.co_name, caller.f_locals["i"],
                   caller.f_locals["marker"]))
    caller.f_locals["marker"] = "changed"
    return self._value


Properties.value.fget.__code__ = observing_property.__code__
for _ in range(2):
    assert read(properties, 4) == (72, "changed")
expected = [("read", i, "initial" if i == 0 else "changed")
            for _ in range(2) for i in range(4)]
assert events == expected, events
Properties.value.fget.__code__ = original_property_code


def observing_lookup(self, name):
    caller = sys._getframe(1)
    events.append((caller.f_code.co_name, caller.f_locals["i"], name))
    if name == "value":
        return 7
    if name == "other":
        return 11
    raise AttributeError(name)


Missing.__getattr__ = observing_lookup
events.clear()
assert read(missing, 5) == (90, "initial")
assert events == [("read", i, name) for i in range(5)
                  for name in ("value", "other")], events
Missing.__getattr__ = original_getattr
try:
    missing.absent
except AttributeError as exc:
    assert str(exc) == "absent"
else:
    raise AssertionError("unhandled getter branch didn't raise")

# Observers need every real getter call, even after native warmup.
events.clear()


def profile(frame, event, arg):
    if event == "call" and frame.f_code.co_name in ("value", "other", "__getattr__"):
        events.append(frame.f_code.co_name)


sys.setprofile(profile)
try:
    assert read(properties, 10) == (180, "initial")
    assert read(missing, 10) == (180, "initial")
finally:
    sys.setprofile(None)
assert events.count("value") == 10, events
assert events.count("other") == 10, events
assert events.count("__getattr__") == 20, events

# A reached class override must run in the getter's real frame.
class Meta(type):
    def __getattribute__(cls, name):
        if name == "item":
            events.append(sys._getframe(1).f_code.co_name)
            return 7
        return super().__getattribute__(name)


class Target(metaclass=Meta):
    item = 0


class ClassReader(Properties):
    @property
    def value(self):
        return Target.item


events.clear()
assert read(ClassReader(), 120) == (2160, "initial")
assert events == ["value"] * 120, events

# Descriptor-valued hooks keep their binding behavior.
class Hook:
    def __get__(self, instance, owner):
        events.append("bind")
        return lambda name: 7 if name == "value" else 11


class DescriptorHook:
    __getattr__ = Hook()


events.clear()
assert binding_read(DescriptorHook(), 5) == 90
assert events == ["bind"] * 10, events

# Invalid binding and suspended bodies can't be treated as ordinary returns.
class BadBinding:
    @property
    def value(self, required):
        return 7


try:
    binding_read(BadBinding(), 1)
except TypeError:
    pass
else:
    raise AssertionError("getter skipped a required argument")


class Suspended:
    @property
    def value(self):
        yield 7


assert list(suspended_read(Suspended(), 1)) == [7]

# A colliding Python key prevents speculative name lookup. The dictionary
# owns a real value, so __getattr__ must not supply its usual answer.
class Key:
    def __hash__(self):
        return hash("value")

    def __eq__(self, name):
        events.append(sys._getframe(1).f_code.co_name)
        return name == "value"


events.clear()
missing.__dict__[Key()] = 23
assert key_read(missing, 3) == 102
assert events and all(name == "key_read" for name in events), events
print("borrowed getter paths: ok")
