"""Cached native dynamic reads retain Python's mutation and fallback rules."""

import types
import math as module


class Node:
    def __init__(self, next=None, value=7, other=3):
        self.next = next
        self.value = value
        self.other = other


class SlotNode:
    __slots__ = ("next", "value", "other")

    def __init__(self, next=None, value=11, other=5):
        self.next = next
        self.value = value
        self.other = other


def make_chain(kind):
    root = kind()
    for _ in range(15):
        root = kind(root)
    return root


def dict_read(root, n):
    total = 0
    for _ in range(n):
        total += root.next.next.next.next.next.next.next.next.next.next.next.next.next.next.next.value
        total += root.next.next.next.next.next.next.next.next.next.next.next.next.next.next.next.other
    return total


def slot_read(root, n):
    total = 0
    for _ in range(n):
        total += root.next.next.next.next.next.next.next.next.next.next.next.next.next.next.next.value
    return total


def class_read(root, n):
    total = 0
    for _ in range(n):
        total += root.value
    return total


def module_read(root, n):
    total = 0
    for _ in range(n):
        total += root.value
    return total


class Constants:
    value = 13


dict_root = make_chain(Node)
slot_root = make_chain(SlotNode)
module.value = 17
assert dict_read(dict_root, 1200) == 12000
assert slot_read(slot_root, 1200) == 13200
assert class_read(Constants, 1200) == 15600
assert module_read(module, 1200) == 20400

# DYNAMIC CACHE MUTATIONS: Rust checks native use before these changes.

def leaf(root):
    for _ in range(15):
        root = root.next
    return root


last = leaf(dict_root)
del last.value
last.shift = 99
last.value = 19
last.other = 23
assert dict_read(dict_root, 29) == 1218
last_slot = leaf(slot_root)
del last_slot.value
try:
    slot_read(slot_root, 1)
except AttributeError as exc:
    assert "value" in str(exc)
else:
    raise AssertionError("unset slot didn't raise")
last_slot.value = 31
assert slot_read(slot_root, 29) == 899
Constants.value = 37
module.value = 41
assert class_read(Constants, 29) == 1073
assert module_read(module, 29) == 1189

seen = []


def get_value(self):
    seen.append(self)
    return 43


Node.value = property(get_value)
assert dict_read(dict_root, 19) == 1254
assert len(seen) == 19 and all(item is last for item in seen)
del Node.value


class ModuleOverride(types.ModuleType):
    def __getattribute__(self, name):
        if name == "value":
            seen.append("module override")
            return 47
        return super().__getattribute__(name)


python_module = types.ModuleType("python_module")
python_module.value = 41
python_module.__class__ = ModuleOverride
assert module_read(python_module, 13) == 611
assert seen[-13:] == ["module override"] * 13


class ModuleProperty(types.ModuleType):
    @property
    def value(self):
        seen.append("module property")
        return 59


# Imported modules have a separate runtime representation. A class change
# plus removal of the cached key must reach its descriptor on a miss.
del module.value
module.__class__ = ModuleProperty
assert module_read(module, 11) == 649
assert seen[-11:] == ["module property"] * 11
module.__class__ = types.ModuleType


class Meta(type):
    def __getattribute__(self, name):
        if name == "value":
            seen.append("metaclass override")
            return 53
        return super().__getattribute__(name)


class Custom(metaclass=Meta):
    value = 0


assert class_read(Custom, 17) == 901
assert seen[-17:] == ["metaclass override"] * 17


# Native method capture preserves both bound and receiver-free calls.
def method_reads(root, n):
    total = 0
    for _ in range(n):
        items = root.items
        total += len(items())
        fromkeys = root.fromkeys
        total += fromkeys(("key",), 7)["key"]
    return total


assert method_reads({"a": 1, "b": 2}, 1200) == 10800
assert method_reads({"a": 1, "b": 2}, 1200) == 10800

print("dynamic attribute cache: ok")
