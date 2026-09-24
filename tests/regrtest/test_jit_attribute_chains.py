"""Consecutive guarded reads must preserve mutation and fallback semantics."""


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


def dict_chain(root, n):
    total = 0
    for _ in range(n):
        total += root.next.next.next.value
    return total


def slot_chain(root, n):
    total = 0
    for _ in range(n):
        total += root.next.next.next.value
    return total


def object_chain(root, n):
    result = root
    for _ in range(n):
        result = root.next.next.next
    return result


def text_chain(root, n):
    result = root.next.next.next.value
    for _ in range(n):
        result = root.next.next.next.value
    return result


dict_root = Node(Mid(Node(Node(value=7))))
slot_root = SlotNode(SlotNode(SlotNode(SlotNode(value=11))))
text = "".join(["attribute ", "chain ", "result"])
text_root = Node(Node(Node(Node(value=text))))
assert dict_chain(dict_root, 1200) == 8400
assert slot_chain(slot_root, 1200) == 13200
assert object_chain(dict_root, 1200) is dict_root.next.next.next
assert text_chain(text_root, 1200) is text

# CHAIN MUTATIONS: the Rust wrapper checks driver compilation here.

# Nested slot borrows can all refer to the same instance.
self_root = SlotNode(value=5)
self_root.next = self_root
assert slot_chain(self_root, 37) == 185

# Final owning pins retain identity, including a nullable object result.
leaf = dict_root.next.next.next
dict_root.next.next.next = None
assert object_chain(dict_root, 31) is None
dict_root.next.next.next = leaf
assert object_chain(dict_root, 31) is leaf
for suffix in ("first", "second", "third"):
    changed = "".join([text, ": ", suffix])
    text_root.next.next.next.value = changed
    assert text_chain(text_root, 31) is changed

dict_root.next.next = Node(Node(value=13))
slot_root.next.next.next.value = 17
assert dict_chain(dict_root, 31) == 403
assert slot_chain(slot_root, 31) == 527

# Reorder the instance dictionary so an old index names a different key.
middle = dict_root.next.next
child = middle.next
del middle.next
middle.extra = "shifted"
middle.next = child
assert dict_chain(dict_root, 29) == 377

# A descriptor introduced at the second step runs exactly once per read.
seen = []


def get_next(self):
    seen.append(self)
    return self.__dict__["next"]


Mid.next = property(get_next)
assert dict_chain(dict_root, 19) == 247
assert len(seen) == 19
assert all(item is dict_root.next for item in seen)
del Mid.next

# Missing and unset final attributes retain their real AttributeError.
for root, driver in ((dict_root, dict_chain), (slot_root, slot_chain)):
    leaf = root.next.next.next
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
        raise AssertionError("missing final attribute didn't raise")
    leaf.value = old
    assert driver(root, 23) == old * 23

# A later intermediate can change lane, including a nullable field.
old = dict_root.next.next
dict_root.next.next = None
try:
    dict_chain(dict_root, 1)
except AttributeError as exc:
    assert "next" in str(exc)
else:
    raise AssertionError("None intermediate didn't raise")
dict_root.next.next = old
assert dict_chain(dict_root, 17) == 221


def paused(root):
    for _ in range(40):
        yield root.next.next.next.value


it = paused(dict_root)
assert [next(it) for _ in range(20)] == [13] * 20
dict_root.next.next.next.value = 29
assert list(it) == [29] * 20

# Discarded intermediate nodes must die during the same native activation.
import gc
import weakref


def replace_chain(root, watch, n):
    total = 0
    collected = False
    for i in range(n):
        total += root.next.next.next.value
        if i == 100:
            root.next = Node(Node(Node(value=31)))
        if i == 200:
            gc.collect()
            collected = watch() is None
    return total, collected


root = Node(Node(Node(Node(value=7))))
watch = weakref.ref(root.next)
assert replace_chain(root, watch, 400) == (9976, True)
print("native attribute chains: ok")
