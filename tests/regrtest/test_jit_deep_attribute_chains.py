"""Eight-field native reads preserve deeper guards and object lifetimes."""

import gc
import weakref


class Node:
    def __init__(self, next=None, value=0):
        self.next = next
        self.value = value


class Mid(Node):
    pass


class SlotNode:
    __slots__ = ("next", "value", "__weakref__")

    def __init__(self, next=None, value=0):
        self.next = next
        self.value = value


def make_chain(depth, value, kind=Node):
    root = kind(value=value)
    for _ in range(depth - 1):
        root = kind(root)
    return root


def dict_read(root, n):
    total = 0
    for _ in range(n):
        total += root.next.next.next.next.next.next.next.value
    return total


def slot_read(root, n):
    total = 0
    for _ in range(n):
        total += root.next.next.next.next.next.next.next.value
    return total


def mixed_read(root, n):
    total = 0
    for _ in range(n):
        total += root.next.next.next.next.next.next.next.value
    return total


def float_read(root, n):
    total = 0.0
    for _ in range(n):
        total += root.next.next.next.next.next.next.next.value
    return total


def object_read(root, n):
    result = root
    for _ in range(n):
        result = root.next.next.next.next.next.next.next.value
    return result


def bytes_read(root, n):
    result = root.next.next.next.next.next.next.next.value
    for _ in range(n):
        result = root.next.next.next.next.next.next.next.value
    return result


dict_root = make_chain(8, 7)
middle = dict_root.next.next.next.next
middle.next = Mid(middle.next.next)
slot_root = make_chain(8, 11, SlotNode)
mixed_root = Node(SlotNode(Node(SlotNode(Node(SlotNode(Node(SlotNode(value=13))))))))
float_root = make_chain(8, 3.5)
object_value = Node(value=17)
object_root = make_chain(8, object_value)
bytes_value = bytes([1, 2, 3, 4])
bytes_root = make_chain(8, bytes_value)
assert dict_read(dict_root, 1200) == 8400
assert slot_read(slot_root, 1200) == 13200
assert mixed_read(mixed_root, 1200) == 15600
assert float_read(float_root, 1200) == 4200.0
assert object_read(object_root, 1200) is object_value
assert bytes_read(bytes_root, 1200) is bytes_value

# DEEP CHAIN MUTATIONS: the Rust wrapper verifies native compilation here.

# A descriptor at the sixth node must run once, after replaying the prefix.
seen = []


def get_next(self):
    seen.append(self)
    return self.__dict__["next"]


Mid.next = property(get_next)
assert dict_read(dict_root, 23) == 161
assert len(seen) == 23
assert all(item is middle.next for item in seen)
del Mid.next

# An indexed key can move late in the chain.
late = dict_root.next.next.next.next.next.next
child = late.next
del late.next
late.extra = "shifted"
late.next = child
assert dict_read(dict_root, 29) == 203

for root, driver in ((dict_root, dict_read), (slot_root, slot_read)):
    leaf = root.next.next.next.next.next.next.next
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
        raise AssertionError("missing deep field didn't raise")
    leaf.value = old
    assert driver(root, 31) == old * 31

late.next = None
try:
    dict_read(dict_root, 1)
except AttributeError as exc:
    assert "value" in str(exc)
else:
    raise AssertionError("nullable deep intermediate didn't raise")
late.next = child
assert dict_read(dict_root, 17) == 119

object_root.next.next.next.next.next.next.next.value = None
assert object_read(object_root, 37) is None
object_root.next.next.next.next.next.next.next.value = object_value
assert object_read(object_root, 37) is object_value
bytes_value = bytes([5, 6, 7, 8])
bytes_root.next.next.next.next.next.next.next.value = bytes_value
assert bytes_read(bytes_root, 37) is bytes_value
float_root.next.next.next.next.next.next.next.value = 9
assert float_read(float_root, 37) == 333.0

# Repeated borrows can all point to the same slotted instance.
self_root = SlotNode(value=5)
self_root.next = self_root
assert slot_read(self_root, 41) == 205


def replace_chain(root, watch, n):
    total = 0
    collected = False
    for i in range(n):
        total += root.next.next.next.next.next.next.next.value
        if i == 100:
            root.next.next.next.next = make_chain(4, 31)
        if i == 200:
            gc.collect()
            collected = watch() is None
    return total, collected


root = make_chain(8, 7)
watch = weakref.ref(root.next.next.next.next)
assert replace_chain(root, watch, 400) == (9976, True)
print("deep native attribute chains: ok")
