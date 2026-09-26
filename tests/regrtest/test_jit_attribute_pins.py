"""Repeated native attribute reads preserve identity across activation changes."""

import gc
import weakref


class Node:
    def __init__(self, next=None, value=0):
        self.next = next
        self.value = value


def chain_total(root, n):
    total = 0
    for _ in range(n):
        total += root.next.next.value
    return total


a = Node(Node(Node(value=3)))
b = Node(Node(Node(value=7)))
for _ in range(80):
    assert chain_total(a, 200) == 600
    assert chain_total(b, 200) == 1400
assert chain_total(a, 20000) == 60000
a.next.next = Node(value=11)
assert chain_total(a, 20000) == 220000
assert chain_total(b, 20000) == 140000

# New activations reuse guard metadata but have different pin tables.
for value in range(50):
    assert chain_total(Node(Node(Node(value=value))), 300) == 300 * value


def rotate(root, other, n):
    total = 0
    for _ in range(n):
        old = root.next
        root.next = other
        other = old
        total += root.next.value
    return total, root.next, other


left, right = Node(value=3), Node(value=7)
root = Node(left)
for _ in range(80):
    result = rotate(root, right, 200)
    assert result == (1000, left, right)
result = rotate(root, right, 20001)
assert result == (100007, right, left)

# Shared compiled code can execute recursively with different receivers.
def recursive(root, other, depth):
    total = 0
    for i in range(100):
        total += root.next.value
        if i == 50 and depth:
            total += recursive(other, root, depth - 1)
        total += root.next.value
    return total


left, right = Node(Node(value=3)), Node(Node(value=7))
for _ in range(80):
    assert recursive(left, right, 2) == 2600
    assert recursive(right, left, 1) == 2000

# Suspended native activations can interleave uses of the same guard hints.
def generated(root):
    for _ in range(4):
        total = 0
        for _ in range(3000):
            total += root.next.value
        yield total


for _ in range(20):
    first, second = generated(left), generated(right)
    assert next(first) == 9000
    assert next(second) == 21000
    left.next = Node(value=5)
    assert next(first) == 15000
    assert next(second) == 21000
    assert list(first) == [15000, 15000]
    assert list(second) == [21000, 21000]
    left.next = Node(value=3)

# Slot-backed reads retain the same identity and name checks.
class Slotted:
    __slots__ = ('next', 'value')

    def __init__(self, next=None, value=0):
        self.next = next
        self.value = value


for _ in range(80):
    assert chain_total(Slotted(Slotted(Slotted(value=13))), 300) == 3900

# A class mutation must invalidate the read even when the pin hint hits.
root = Node(Node(Node(value=19)))
for _ in range(80):
    assert chain_total(root, 300) == 5700
Node.value = property(lambda self: self.__dict__['value'] + 1)
assert chain_total(root, 20000) == 400000
del Node.value
assert chain_total(root, 20000) == 380000

# Equal strings and bytes can be distinct objects. Reuse must check identity.
class TextBox:
    def __init__(self, text, data):
        self.text = text
        self.data = data


def read_text(box, n):
    text = box.text
    for _ in range(n):
        text = box.text
    return text


def read_bytes(box, n):
    data = box.data
    for _ in range(n):
        data = box.data
    return data


box = TextBox(''.join(['some', ' text']), bytes([11, 22, 33]))
for _ in range(80):
    assert read_text(box, 300) is box.text
    assert read_bytes(box, 300) is box.data
old_text, old_bytes = box.text, box.data
box.text = ''.join(['some', ' text'])
box.data = bytes([11, 22, 33])
assert box.text == old_text and box.text is not old_text
assert box.data == old_bytes and box.data is not old_bytes
assert read_text(box, 20000) is box.text
assert read_bytes(box, 20000) is box.data

# Hints must not own the last result after an activation has retired.
root = Node(Node(Node(value=17)))
refs = [weakref.ref(root), weakref.ref(root.next), weakref.ref(root.next.next)]
assert chain_total(root, 20000) == 340000
del root
gc.collect()
assert all(ref() is None for ref in refs)
print('native attribute pins: ok')
