"""Borrow long cached chains without changing fallback or intermediate lifetime."""

import gc
import math as module_link
import types
import weakref


class Node:
    def __init__(self, next=None, value=7):
        self.next = next
        self.value = value


class SlotNode:
    __slots__ = ("next", "value", "__weakref__")

    def __init__(self, next=None, value=7):
        self.next = next
        self.value = value


def make_chain(depth, value=7, kind=Node):
    root = kind(value=value)
    for _ in range(depth - 1):
        root = kind(root)
    return root


def at(root, depth):
    for _ in range(depth):
        root = root.next
    return root


def dict_read(root, n):
    total = 0
    for _ in range(n):
        total += root.next.next.next.next.next.next.next.next.next.next.next.next.next.next.next.value
    return total


def slot_read(root, n):
    total = 0
    for _ in range(n):
        total += root.next.next.next.next.next.next.next.next.next.next.next.next.next.next.next.value
    return total


def mixed_read(root, n):
    total = 0
    for _ in range(n):
        total += root.next.next.next.next.next.next.next.next.next.next.next.next.next.next.next.value
    return total


def object_read(root, n):
    value = None
    for _ in range(n):
        value = root.next.next.next.next.next.next.next.next.next.next.next.next.next.next.next.value
    return value


def class_read(root, n):
    total = 0
    for _ in range(n):
        total += root.next.next.next.next.next.next.next.next.next.next.next.next.next.next.next.value
    return total


def module_read(root, n):
    total = 0
    for _ in range(n):
        total += root.next.next.next.next.next.next.next.next.next.next.next.next.next.next.next.value
    return total


def getter_read(root, n):
    total = 0
    for _ in range(n):
        total += root.next.next.next.next.next.next.next.next.next.next.next.next.next.next.next.value
    return total


class Link:
    @property
    def next(self):
        return self.child


class Constant:
    next = make_chain(7)


dict_root = make_chain(16)
slot_root = make_chain(16, 11, SlotNode)
mixed_root = SlotNode(value=13)
for i in range(15):
    mixed_root = (Node if i % 2 else SlotNode)(mixed_root)
object_value = Node(value=19)
object_root = make_chain(16, object_value)
class_root = make_chain(16)
at(class_root, 7).next = Constant
module_root = make_chain(16)
module_link.next = make_chain(7)
at(module_root, 7).next = module_link
getter_root = make_chain(16)
getter_link = Link()
getter_link.child = make_chain(7)
at(getter_root, 7).next = getter_link
for _ in range(2):
    assert dict_read(dict_root, 3000) == 21000
    assert slot_read(slot_root, 3000) == 33000
    assert mixed_read(mixed_root, 3000) == 39000
    assert object_read(object_root, 3000) is object_value
    assert class_read(class_root, 3000) == 21000
    assert module_read(module_root, 3000) == 21000
    assert getter_read(getter_root, 3000) == 21000

# Exercise both sides of the bounded walk with distinct compiled bodies.
for depth in (9, 17, 32, 33):
    name = "boundary_" + str(depth)
    expr = "root" + ".next" * (depth - 1) + ".value"
    exec("def " + name + "(root, n):\n"
         "    total = 0\n"
         "    for _ in range(n):\n"
         "        total += " + expr + "\n"
         "    return total\n")
    driver = globals()[name]
    root = make_chain(depth)
    assert driver(root, 3000) == 21000
    assert driver(root, 3000) == 21000

# CACHED CHAIN MUTATIONS: verify compiled drivers and native counters first.

# The borrowed helper may decline; the original dynamic helpers stay usable.
Constant.next = make_chain(7, 17)
module_link.next = make_chain(7, 19)
getter_link.child = make_chain(7, 23)
assert class_read(class_root, 31) == 527
assert module_read(module_root, 31) == 589
# Python-created modules take the ordinary object protocol on this runtime.
python_module = types.ModuleType("chain_link")
python_module.next = make_chain(7, 29)
at(module_root, 7).next = python_module
assert module_read(module_root, 31) == 899
at(module_root, 7).next = module_link
assert getter_read(getter_root, 31) == 713

# Descriptors on either side of the guarded boundary run exactly once. Their
# caller is the actual reader with its current loop locals, not the module.
import sys
seen = []


def next_descriptor(self):
    caller = sys._getframe(1)
    seen.append((caller.f_code.co_name, caller.f_locals["root"]))
    return self.__dict__["next"]


for position in (6, 10):
    class Changed(Node):
        pass

    root = make_chain(16)
    old = at(root, position)
    at(root, position - 1).next = Changed(old.next)
    assert dict_read(root, 3000) == 21000
    seen.clear()
    Changed.next = property(next_descriptor)
    assert dict_read(root, 23) == 161
    assert seen == [("dict_read", root)] * 23
    del Changed.next

# Key reordering and missing fields must use the exact current names.
late = at(dict_root, 12)
child = late.next
del late.next
late.padding = "shifted"
late.next = child
assert dict_read(dict_root, 29) == 203
for root, driver in ((dict_root, dict_read), (slot_root, slot_read)):
    leaf = at(root, 15)
    old = leaf.value
    del leaf.value
    try:
        driver(root, 1)
    except AttributeError as exc:
        assert "value" in str(exc)
        tb = exc.__traceback__
        while tb.tb_next is not None:
            tb = tb.tb_next
        assert tb.tb_frame.f_code is driver.__code__
    else:
        raise AssertionError("missing final field didn't raise")
    leaf.value = old
    assert driver(root, 31) == old * 31

late.next = None
try:
    dict_read(dict_root, 1)
except AttributeError as exc:
    assert "next" in str(exc)
else:
    raise AssertionError("nullable intermediate didn't raise")
late.next = child

# The final owning result accepts every ordinary object lane and preserves
# identity. Integer specialization must decline when the result changes type.
for value in (None, 3.5, b"changed", [1, 2], object_value):
    at(object_root, 15).value = value
    assert object_read(object_root, 37) is value
at(dict_root, 15).value = 2.5
assert dict_read(dict_root, 38) == 95.0
at(dict_root, 15).value = 7
self_root = SlotNode(value=5)
self_root.next = self_root
assert slot_read(self_root, 41) == 205


def replace_chain(root, watch, n):
    total = 0
    collected = False
    for i in range(n):
        total += root.next.next.next.next.next.next.next.next.next.next.next.next.next.next.next.value
        if i == 100:
            root.next.next.next.next.next.next.next.next = make_chain(8, 31)
        if i == 200:
            gc.collect()
            collected = watch() is None
    return total, collected


# Warm gc.collect's attribute cache before testing collection in the same
# activation. A cold cache can deopt early and hide an intermediate owner.
warm = make_chain(16)
replace_chain(warm, weakref.ref(at(warm, 8)), 400)
root = make_chain(16)
watch = weakref.ref(at(root, 8))
assert replace_chain(root, watch, 400) == (9976, True)


# Finalizers and weakref callbacks must not be delayed by intermediate pins.
events = []


class FinalNode(Node):
    def __del__(self):
        events.append("finalized")


def make_final_root(value=7):
    root = make_chain(16, value)
    at(root, 7).next = FinalNode(make_chain(7, value))
    return root


def make_final_tail(value):
    return FinalNode(make_chain(7, value))


def replace_final_chain(root, watch, n):
    total = 0
    collected = False
    for i in range(n):
        total += root.next.next.next.next.next.next.next.next.next.next.next.next.next.next.next.value
        if i == 100:
            root.next.next.next.next.next.next.next.next = make_final_tail(31)
        if i == 200:
            gc.collect()
            collected = watch() is None
    return total, collected


# Keep the eighth link's class stable across warmup and replacement. Cold
# cache misses retain the original helpers' separate ownership behavior.
warm = make_final_root()
replace_final_chain(warm, weakref.ref(at(warm, 8)), 400)
root = make_final_root()
watch = weakref.ref(at(root, 8), lambda ref: events.append("weakref"))
events.clear()
assert replace_final_chain(root, watch, 400) == (9976, True)
assert events.count("finalized") == 1
assert events.count("weakref") == 1


def suspended(root):
    yield root.next.next.next.next.next.next.next.next.next.next.next.next.next.next.next.value
    yield root.next.next.next.next.next.next.next.next.next.next.next.next.next.next.next.value


generator = suspended(object_root)
assert next(generator) is object_value
new_value = Node(value=29)
at(object_root, 15).value = new_value
gc.collect()
assert next(generator) is new_value
print("cached native attribute chains: ok")
