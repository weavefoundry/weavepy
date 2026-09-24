"""Consecutive cached reads preserve mutation, callbacks, and ownership."""

import gc
import sys
import weakref


class Node:
    pass


class Payload:
    pass


class Slotted:
    __slots__ = ("next", "value")


def read_slots_4(root):
    result = root.next.next.next.value
    return result


def read_mixed_4(root):
    result = root.next.next.next.value
    return result


def make_chain(depth, value, padding=False):
    tail = Node()
    tail.value = value
    nodes = [tail]
    for index in range(depth - 1):
        parent = Node()
        if padding:
            parent.padding = index
        parent.next = nodes[-1]
        nodes.append(parent)
    nodes.reverse()
    return nodes


readers = {}
for depth in (2, 3, 4, 8, 9, 12):
    # STORE_FAST keeps this out of the separate pure-leaf evaluator.
    source = ("def read_%d(root):\n    result = root%s.value\n"
              "    return result\n") % (depth, ".next" * (depth - 1))
    exec(source, globals())
    readers[depth] = globals()["read_%d" % depth]

payload = Payload()
chains = {depth: make_chain(depth, payload) for depth in readers}
for _ in range(1000):
    for depth, reader in readers.items():
        assert reader(chains[depth][0]) is payload

slot_nodes = [Slotted() for _ in range(4)]
mixed_nodes = [Node(), Slotted(), Node(), Slotted()]
for nodes in (slot_nodes, mixed_nodes):
    for index in range(3):
        nodes[index].next = nodes[index + 1]
    nodes[-1].value = payload
slot_root = slot_nodes[0]
mixed_root = mixed_nodes[0]
for _ in range(1000):
    assert read_slots_4(slot_root) is payload
    assert read_mixed_4(mixed_root) is payload

# INTERPRETER CHAIN MUTATIONS:
for depth, reader in readers.items():
    nodes = chains[depth]
    for node in nodes[:-1]:
        child = node.next
        del node.next
        node.padding = "moves the cached index"
        node.next = child
        assert reader(nodes[0]) is payload
        node.__dict__ = {"unrelated": None, "next": child}
        assert reader(nodes[0]) is payload
    nodes[-1].value = 37
    assert reader(nodes[0]) == 37
    nodes[-1].value = None
    assert reader(nodes[0]) is None
    del nodes[-1].value
    try:
        reader(nodes[0])
    except AttributeError:
        pass
    else:
        raise AssertionError("a missing final field must raise")
    nodes[-1].value = payload
    assert reader(nodes[0]) is payload

# A later descriptor must run once with the real reader frame, even after
# earlier ordinary fields have been fused into a completed prefix.
events = []


class Observed:
    @property
    def next(self):
        frame = sys._getframe(1)
        events.append((frame.f_code.co_name, frame.f_locals["root"]))
        return self.child


nodes = chains[4]
observed = Observed()
observed.child = nodes[3]
nodes[1].next = observed
assert read_4(nodes[0]) is payload
assert events == [("read_4", nodes[0])]
nodes[1].next = nodes[2]
assert read_4(nodes[0]) is payload


class Different:
    @property
    def value(self):
        events.append("replacement class")
        return 51


tail = chains[3][-1]
tail.__class__ = Different
assert read_3(chains[3][0]) == 51
assert events[-1] == "replacement class"
tail.__class__ = Node
assert read_3(chains[3][0]) is payload

# Replacing the original class's hook invalidates every old field guard.
events.clear()


def custom_access(self, name):
    events.append(name)
    return object.__getattribute__(self, name)


Node.__getattribute__ = custom_access
assert read_3(chains[3][0]) is payload
assert events == ["next", "next", "value"]
del Node.__getattribute__


slotted = [Slotted(), Slotted(), Slotted()]
slotted[0].next = slotted[1]
slotted[1].next = slotted[2]
slotted[2].value = payload
assert read_3(slotted[0]) is payload
assert read_3(chains[3][0]) is payload

for reader, nodes in ((read_slots_4, slot_nodes), (read_mixed_4, mixed_nodes)):
    # Deletion can shift a populated slot's cached index. A missing field
    # must still raise before later reads, then recover after reassignment.
    for index in range(3):
        child = nodes[index].next
        del nodes[index].next
        nodes[index].value = "changes the populated slot order"
        try:
            reader(nodes[0])
        except AttributeError:
            pass
        else:
            raise AssertionError("a missing intermediate field must raise")
        nodes[index].next = child
        assert reader(nodes[0]) is payload
    nodes[-1].value = None
    assert reader(nodes[0]) is None
    nodes[-1].value = payload

# A slot descriptor replaced by a property invalidates the class guard.
original_next = Slotted.next
events.clear()


def replacement_next(self):
    events.append(sys._getframe(1).f_code.co_name)
    return original_next.__get__(self, Slotted)


Slotted.next = property(replacement_next)
try:
    assert read_slots_4(slot_root) is payload
    assert events == ["read_slots_4"] * 3
finally:
    Slotted.next = original_next
assert read_slots_4(slot_root) is payload

# Warm readers retain their result, not their intermediate receivers.
owned_payload = Payload()
owned_nodes = make_chain(4, owned_payload)
watches = [weakref.ref(node) for node in owned_nodes]
payload_watch = weakref.ref(owned_payload)
for _ in range(1000):
    result = read_4(owned_nodes[0])
owned_nodes = owned_payload = None
gc.collect()
assert all(watch() is None for watch in watches)
assert payload_watch() is result
del result
gc.collect()
assert payload_watch() is None

# Observer mode sees the original instructions and the correct frame.
traced = []


def trace(frame, event, arg):
    if frame.f_code.co_name == "read_3":
        traced.append(event)
    return trace


sys.settrace(trace)
try:
    assert read_3(chains[3][0]) is payload
finally:
    sys.settrace(None)
assert "call" in traced and "line" in traced and "return" in traced
print("interpreter attribute chains: ok")
